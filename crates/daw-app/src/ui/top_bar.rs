//! The bar across the top of the window: wordmark, project and menus on
//! the left, then the transport and its readouts, and on the right the
//! audio thread's load and the MCP address.

use super::state::{Cx, DetailTab};
use super::{arrangement, files, piano_roll, timeline};
use daw_core::project::{MAX_TEMPO, MIN_TEMPO};
use daw_noir::{Chip, Icon, MenuItem, NumberField, TextStyle, Theme, paint, text};
use eframe::egui::{self, Align2, Id, Rect, Sense, Ui, pos2, vec2};

pub const HEIGHT: f32 = 36.0;
const EDGE: f32 = 10.0;
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
    3.0 * BUTTON_WIDTH + 2.0 * 4.0 + 10.0 + POSITION_WIDTH + 6.0 + TEMPO_WIDTH + 6.0 + SIGNATURE_WIDTH;
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

    x += text::paint(&painter, row(x), Align2::LEFT_CENTER, theme.text.brand, "DAW", theme.ink) + GAP;

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
        let response = daw_noir::menu_title(ui, theme, rect, ui.scope_id().with(("menu", title)), title);
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
    if MenuItem::new("New project").shortcut("Ctrl+N").show(ui, theme).clicked() {
        files::new_project(cx);
    }
    if MenuItem::new("Open\u{2026}").shortcut("Ctrl+O").show(ui, theme).clicked() {
        files::open(ui.ctx(), cx);
    }
    daw_noir::menu_separator(ui, theme);
    if MenuItem::new("Save").shortcut("Ctrl+S").show(ui, theme).clicked() {
        files::save(ui.ctx(), cx);
    }
    if MenuItem::new("Save as\u{2026}").shortcut("Ctrl+Shift+S").show(ui, theme).clicked() {
        files::save_as(ui.ctx(), cx);
    }
    daw_noir::menu_separator(ui, theme);
    let idle = cx.jobs.exporting().is_none();
    if MenuItem::new("Export WAV\u{2026}").shortcut("Ctrl+E").enabled(idle).show(ui, theme).clicked() {
        files::export_wav(ui.ctx(), cx);
    }
}

fn edit_menu(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    if MenuItem::new("Undo").shortcut("Ctrl+Z").enabled(cx.snap.can_undo).show(ui, theme).clicked() {
        cx.with_session(|session| session.undo());
    }
    if MenuItem::new("Redo").shortcut("Ctrl+Shift+Z").enabled(cx.snap.can_redo).show(ui, theme).clicked() {
        cx.with_session(|session| session.redo());
    }
    daw_noir::menu_separator(ui, theme);
    let clip = cx.state.selected_clip.is_some();
    if MenuItem::new("Duplicate clip").shortcut("Ctrl+D").enabled(clip).show(ui, theme).clicked() {
        arrangement::duplicate_selected_clip(cx);
    }
    if MenuItem::new("Delete clip").shortcut("Del").enabled(clip).show(ui, theme).clicked() {
        arrangement::delete_selected_clip(cx);
    }
    let notes = clip && cx.state.detail_tab == DetailTab::Clip;
    if MenuItem::new("Select all notes").shortcut("Ctrl+A").enabled(notes).show(ui, theme).clicked() {
        piano_roll::select_all_notes(cx);
    }
    daw_noir::menu_separator(ui, theme);
    if MenuItem::new("Add track").shortcut("Ctrl+T").show(ui, theme).clicked() {
        arrangement::add_track(cx);
    }
}

fn view_menu(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let open = cx.state.browser.open;
    if MenuItem::new("Browser").shortcut("Ctrl+B").checked(open).show(ui, theme).clicked() {
        cx.state.browser.open = !open;
    }
    daw_noir::menu_separator(ui, theme);
    for (tab, label) in [(DetailTab::Clip, "Clip"), (DetailTab::Devices, "Devices")] {
        if MenuItem::new(label).checked(cx.state.detail_tab == tab).show(ui, theme).clicked() {
            cx.state.detail_tab = tab;
        }
    }
    daw_noir::menu_separator(ui, theme);
    daw_noir::menu_heading(ui, theme, "Theme");
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

/// Play, stop, loop, then position, tempo and time signature.
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
    let response = daw_noir::icon_button(ui, theme, button(&mut x), id.with("play"), Icon::Play, playing, color);
    cx.hint(&response, "Play \u{00B7} Space");
    if response.clicked() {
        cx.with_session(|session| session.play());
    }
    let response = daw_noir::icon_button(ui, theme, button(&mut x), id.with("stop"), Icon::Stop, false, theme.ink_dim);
    cx.hint(&response, "Stop \u{00B7} Space \u{00B7} when stopped, returns to the start");
    if response.clicked() {
        stop_or_rewind(cx);
    }
    let looping = cx.snap.project.loop_region.enabled;
    let color = if looping { theme.ink } else { theme.ink_dim };
    let response = daw_noir::icon_button(ui, theme, button(&mut x), id.with("loop"), Icon::Loop, looping, color);
    cx.hint(&response, "Loop the region marked on the ruler \u{00B7} L");
    if response.clicked() {
        toggle_loop(cx);
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
    let output = daw_noir::number_field(ui, theme, boxed(bar, x, TEMPO_WIDTH), id.with("tempo"), &field);
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

    let meter = boxed(bar, x, SIGNATURE_WIDTH);
    paint::frame(&painter, meter, theme.hairline);
    let shown = format!("{} / {}", signature.numerator, signature.denominator);
    text::paint(&painter, meter, Align2::CENTER_CENTER, theme.text.readout, &shown, theme.ink);
    let response = ui.interact(meter, id.with("signature"), Sense::hover());
    cx.hint(&response, "Time signature");
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
        let response = daw_noir::chip(ui, theme, rect, ui.scope_id().with("mcp"), &chip);
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
    daw_noir::mini_meter(&painter, theme, meter, load, color);
    let label_width = text::width(&painter, theme.text.micro, "CPU").ceil();
    let label =
        Rect::from_min_max(pos2(meter.left() - 8.0 - label_width, bar.top()), pos2(meter.left() - 8.0, bar.bottom()));
    text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.micro, "CPU", theme.ink_faint);

    let group = Rect::from_min_max(pos2(label.left(), bar.top()), pos2(right, bar.bottom()));
    let response = ui.interact(group, Id::unique("cpu-load"), Sense::hover());
    cx.hint(&response, "Share of the audio thread\u{2019}s time budget in use");
    label.left()
}
