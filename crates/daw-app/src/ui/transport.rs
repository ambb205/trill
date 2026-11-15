//! The bar across the top of the window: project and file actions on the
//! left, transport and song settings in the middle, engine status on the
//! right.

use super::state::Cx;
use super::widgets::{self, Icon};
use super::{files, theme, timeline};
use daw_core::project::{MAX_TEMPO, MIN_TEMPO};
use eframe::egui::{self, Align, Align2, Color32, Layout, Rect, Sense, Ui, pos2, vec2};

pub const HEIGHT: f32 = 48.0;
/// Width of the middle group, which is laid out at a fixed size so it can
/// be centered in the window.
const CENTER_WIDTH: f32 = 452.0;
const EDGE_PADDING: f32 = 12.0;
/// The project name gets at most this much of the bar.
const TITLE_MAX_WIDTH: f32 = 210.0;
/// Width the status group needs to show everything, output name included.
const STATUS_FULL_WIDTH: f32 = 400.0 + MCP_CHIP_WIDTH + theme::GAP / 2.0;
const MCP_CHIP_WIDTH: f32 = 42.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let bar = ui.max_rect();
    ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, theme::hairline(theme::BORDER));
    let inner = bar.shrink2(vec2(EDGE_PADDING, 0.0));

    let left_end = widgets::place(ui, inner, "project", Layout::left_to_right(Align::Center), |ui| {
        project_group(ui, cx);
        ui.min_rect().right()
    });
    // What is left for the status group once the transport has its place.
    // In a narrow window the output's name is the first thing to go; it
    // stays available as a tooltip.
    let gap = theme::GAP * 2.0;
    let room = inner.right() - (left_end + gap + CENTER_WIDTH + gap);
    let right_start = widgets::place(ui, inner, "status", Layout::right_to_left(Align::Center), |ui| {
        status_group(ui, cx, room >= STATUS_FULL_WIDTH);
        ui.min_rect().left()
    });

    // Centered when there is room, otherwise squeezed between its
    // neighbors, keeping clear of the file actions first.
    let centered = bar.center().x - CENTER_WIDTH / 2.0;
    let left = centered.min(right_start - gap - CENTER_WIDTH).max(left_end + gap);
    let center = Rect::from_min_size(pos2(left, bar.top()), vec2(CENTER_WIDTH, bar.height()));
    widgets::place(ui, center, "transport", Layout::left_to_right(Align::Center), |ui| transport_group(ui, cx));
}

/// App mark, project name, File menu, undo and redo.
fn project_group(ui: &mut Ui, cx: &mut Cx) {
    let (mark, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    ui.painter().rect_filled(mark, 6.0, Color32::from_rgb(0x7C, 0x3A, 0xED));
    ui.painter().text(mark.center(), Align2::CENTER_CENTER, "D", theme::strong(14.0), Color32::WHITE);

    let name = &cx.snap.project.name;
    let title = if cx.snap.modified { format!("{name} \u{2022}") } else { name.clone() };
    let location = match &cx.snap.path {
        Some(path) => path.display().to_string(),
        None => "Not saved yet".to_owned(),
    };
    ui.scope(|ui| {
        ui.set_max_width(TITLE_MAX_WIDTH);
        ui.add(egui::Label::new(egui::RichText::new(&title).font(theme::strong(13.0))).truncate()).on_hover_text(
            if cx.snap.modified {
                format!("{title}\n{location}\nUnsaved changes")
            } else {
                format!("{title}\n{location}")
            },
        );
    });

    ui.add_space(theme::GAP / 2.0);
    file_menu(ui, cx);

    ui.add_space(theme::GAP / 2.0);
    let undo = ui.add_enabled_ui(cx.snap.can_undo, |ui| widgets::icon_button(ui, Icon::Undo, None)).inner;
    if undo.on_hover_text("Undo (Ctrl+Z)").clicked() {
        cx.with_session(|session| session.undo());
    }
    let redo = ui.add_enabled_ui(cx.snap.can_redo, |ui| widgets::icon_button(ui, Icon::Redo, None)).inner;
    if redo.on_hover_text("Redo (Ctrl+Shift+Z)").clicked() {
        cx.with_session(|session| session.redo());
    }
}

fn file_menu(ui: &mut Ui, cx: &mut Cx) {
    ui.menu_button("File", |ui| {
        ui.set_min_width(210.0);
        let item = |ui: &mut Ui, label: &str, shortcut: &str| {
            ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked()
        };
        if item(ui, "New project", "Ctrl+N") {
            files::new_project(cx);
        }
        if item(ui, "Open\u{2026}", "Ctrl+O") {
            files::open(ui.ctx(), cx);
        }
        ui.separator();
        if item(ui, "Save", "Ctrl+S") {
            files::save(ui.ctx(), cx);
        }
        if item(ui, "Save as\u{2026}", "Ctrl+Shift+S") {
            files::save_as(ui.ctx(), cx);
        }
        ui.separator();
        let idle = cx.jobs.exporting().is_none();
        if ui.add_enabled(idle, egui::Button::new("Export WAV\u{2026}").shortcut_text("Ctrl+E")).clicked() {
            files::export_wav(ui.ctx(), cx);
        }
    });
}

/// Play, stop, loop, position, tempo and time signature.
fn transport_group(ui: &mut Ui, cx: &mut Cx) {
    ui.spacing_mut().item_spacing.x = 4.0;

    let playing = cx.live.playing;
    if widgets::icon_button(ui, Icon::Play, playing.then_some(theme::PLAY)).on_hover_text("Play (Space)").clicked() {
        cx.with_session(|session| session.play());
    }
    if widgets::icon_button(ui, Icon::Stop, None)
        .on_hover_text("Stop (Space). When stopped, returns to the start.")
        .clicked()
    {
        stop_or_rewind(cx);
    }
    let region = cx.snap.project.loop_region;
    if widgets::icon_button(ui, Icon::Loop, region.enabled.then_some(theme::ACCENT))
        .on_hover_text("Loop the region marked on the ruler (L)")
        .clicked()
    {
        toggle_loop(cx);
    }

    ui.add_space(theme::GAP);
    let signature = cx.snap.project.time_signature;
    let (readout, _) = ui.allocate_exact_size(vec2(156.0, 30.0), Sense::hover());
    ui.painter().rect_filled(readout, theme::RADIUS, theme::BG_INSET);
    ui.painter().text(
        readout.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        timeline::format_position(cx.live.position, signature),
        theme::strong(16.0),
        if playing { theme::PLAY } else { theme::TEXT },
    );
    ui.painter().text(
        readout.right_center() - vec2(12.0, 0.0),
        Align2::RIGHT_CENTER,
        timeline::format_clock(cx.live.position, cx.snap.project.tempo),
        theme::font(12.0),
        theme::TEXT_DIM,
    );

    ui.add_space(theme::GAP);
    let mut tempo = cx.snap.project.tempo;
    let response = ui
        .add_sized(
            vec2(104.0, 30.0),
            egui::DragValue::new(&mut tempo)
                .range(MIN_TEMPO..=MAX_TEMPO)
                .speed(0.25)
                .max_decimals(2)
                .suffix(" BPM")
                .update_while_editing(false),
        )
        .on_hover_text("Tempo. Drag, or double-click to type.");
    if response.drag_started() {
        cx.begin_gesture();
    }
    if response.changed() {
        cx.edit(|project| project.set_tempo(tempo));
    }

    let (meter, _) = ui.allocate_exact_size(vec2(44.0, 30.0), Sense::hover());
    ui.painter().rect_filled(meter, theme::RADIUS, theme::BG_INSET);
    ui.painter().text(
        meter.center(),
        Align2::CENTER_CENTER,
        format!("{}/{}", signature.numerator, signature.denominator),
        theme::strong(13.0),
        theme::TEXT_DIM,
    );
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

/// Audio output, CPU load, warnings and export progress, right to left.
/// `show_output` is false when the bar is too narrow for the output's name.
fn status_group(ui: &mut Ui, cx: &mut Cx, show_output: bool) {
    let audio = cx.audio;
    if show_output {
        ui.scope(|ui| {
            ui.set_max_width(230.0);
            ui.add(
                egui::Label::new(egui::RichText::new(&audio.output).font(theme::font(12.0)).color(theme::TEXT_DIM))
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text(format!("Audio output: {}", audio.output));
        });
        ui.add_space(theme::GAP / 2.0);
    }
    cpu_meter(ui, cx.live.cpu_load, &audio.output);
    if show_output && let Some(url) = cx.mcp_url {
        ui.add_space(theme::GAP / 2.0);
        mcp_chip(ui, url);
    }

    let issues = issues(cx);
    if !issues.is_empty() {
        ui.add_space(theme::GAP / 2.0);
        let (rect, response) = ui.allocate_exact_size(vec2(44.0, 26.0), Sense::hover());
        ui.painter().rect_filled(rect, theme::RADIUS, theme::fade(theme::WARN, 0.16));
        let icon = Rect::from_center_size(rect.left_center() + vec2(15.0, 0.0), vec2(16.0, 16.0));
        widgets::paint_icon(ui.painter(), icon, Icon::Warning, theme::WARN);
        ui.painter().text(
            rect.right_center() - vec2(9.0, 0.0),
            Align2::RIGHT_CENTER,
            issues.len(),
            theme::strong(12.0),
            theme::WARN,
        );
        response.on_hover_ui(|ui| {
            ui.set_max_width(420.0);
            ui.label(egui::RichText::new("Needs attention").font(theme::strong(13.0)).color(theme::WARN));
            for issue in &issues {
                ui.label(format!("\u{2022} {issue}"));
            }
        });
    }

    if let Some(path) = cx.jobs.exporting() {
        ui.add_space(theme::GAP / 2.0);
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        ui.label(egui::RichText::new("Rendering\u{2026}").color(theme::ACCENT))
            .on_hover_text(format!("Exporting {name}. You can keep working."));
        ui.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
    }
}

/// Says that an AI assistant can drive this window, and where to point it.
fn mcp_chip(ui: &mut Ui, url: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(MCP_CHIP_WIDTH, 22.0), Sense::click());
    let color = if response.hovered() { theme::ACCENT } else { theme::TEXT_DIM };
    ui.painter().rect_stroke(rect, theme::RADIUS, theme::hairline(theme::fade(color, 0.6)), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, "MCP", theme::strong(10.5), color);
    if response.clicked() {
        ui.ctx().copy_text(url.to_owned());
    }
    response.on_hover_text(format!("AI assistants can connect to this session at\n{url}\nClick to copy the address."));
}

/// Everything the warning indicator should tell the user about.
fn issues(cx: &Cx) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(warning) = &cx.audio.warning {
        issues.push(warning.clone());
    }
    issues.extend(cx.snap.problems.iter().cloned());
    if cx.live.audio_faults > 0 {
        issues.push(format!(
            "A device crashed {} time(s) while processing audio; those moments were silenced.",
            cx.live.audio_faults
        ));
    }
    issues
}

fn cpu_meter(ui: &mut Ui, load: f32, output: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(78.0, 26.0), Sense::hover());
    let color = match load {
        load if load >= 0.85 => theme::ERROR,
        load if load >= 0.6 => theme::WARN,
        _ => theme::TEXT_DIM,
    };
    let painter = ui.painter();
    painter.text(rect.left_center(), Align2::LEFT_CENTER, "CPU", theme::font(10.0), theme::TEXT_FAINT);
    let track = Rect::from_min_size(pos2(rect.left() + 26.0, rect.center().y - 2.0), vec2(22.0, 4.0));
    painter.rect_filled(track, 2.0, theme::BG_INSET);
    let filled = track.with_max_x(track.left() + track.width() * load.clamp(0.0, 1.0));
    painter.rect_filled(filled, 2.0, color);
    painter.text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        format!("{:.0}%", load.clamp(0.0, 9.99) * 100.0),
        theme::font(11.0),
        color,
    );
    response.on_hover_text(format!("Share of the audio thread's time budget in use\nAudio output: {output}"));
}
