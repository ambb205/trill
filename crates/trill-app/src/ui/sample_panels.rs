//! What the sample-playing devices play: the sampler's file and the drum
//! rack's sixteen pads.
//!
//! Which file a sampler or a pad plays is not a parameter, so the generic
//! panel cannot show it. Here the sampler gets a column of its own, in
//! front of its parameters, with the file's name and waveform and the
//! start, end and loop points drawn on it. The drum rack gets a panel of
//! its own: the pads as a grid, each with its file, and beside it the
//! controls of the one pad that is selected, instead of all ninety-six at
//! once.
//!
//! Both take sounds dragged from the browser (see [`crate::ui::dragging`]).

use super::browser;
use super::device_layouts::Format;
use super::devices::{self, Body};
use super::sounds::{DropTarget, WaveState, file_stem};
use super::state::{AUDITION_LENGTH, Cx};
use eframe::egui::{self, Align2, Color32, Mesh, Rect, Role, Sense, Shape, Ui, WidgetInfo, pos2, vec2};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use trill_core::{ChainRef, Device, DeviceId, Track};
use trill_dsp::drumrack::{FIRST_NOTE, PAD_COUNT, PAD_NAMES};
use trill_engine::SampleFile;
use trill_noir::{Chip, Icon, MenuItem, fade, paint, text};

pub const SAMPLER: &str = "builtin:sampler";
pub const DRUM_RACK: &str = "builtin:drumrack";

/// Width of the sampler's column that shows its file.
pub(super) const SAMPLE_COLUMN: f32 = 230.0;
/// Size of a pad of the drum rack, and the space between pads.
pub(super) const PAD: egui::Vec2 = egui::Vec2::new(102.0, 56.0);
pub(super) const PAD_GAP: f32 = 3.0;
const PADS_ACROSS: usize = 4;
/// Width of the column of the selected pad's controls, and of the rack's.
const PAD_COLUMN: f32 = 164.0;
const RACK_COLUMN: f32 = 150.0;
const COLUMN_GAP: f32 = 12.0;
const HEADING: f32 = devices::SECTION_HEADER;
/// Height of the waveform in the sampler's column and beside the pads.
const WAVE_HEIGHT: f32 = 84.0;
const PAD_WAVE_HEIGHT: f32 = 50.0;

/// Whether the device at `uri` plays audio files.
pub fn plays_samples(uri: &str) -> bool {
    uri == SAMPLER || uri == DRUM_RACK
}

/// The device of `track` that its notes play: the first instrument of its
/// chain.
pub fn instrument<'a>(cx: &Cx, track: &'a Track) -> Option<&'a Device> {
    track.devices.iter().find(|device| {
        cx.descriptors
            .iter()
            .any(|known| known.uri == device.uri && known.category == trill_core::device::DeviceCategory::Instrument)
    })
}

/// `"36 Kick"`: a pad by its note and what the General MIDI map puts there.
pub fn pad_title(note: u8) -> String {
    match PAD_NAMES.get(usize::from(note.saturating_sub(FIRST_NOTE))) {
        Some(name) if note >= FIRST_NOTE => format!("{note} {name}"),
        _ => note.to_string(),
    }
}

/// Width of the drum rack's panel body.
pub(super) fn drum_rack_width() -> f32 {
    PADS_ACROSS as f32 * PAD.x + (PADS_ACROSS - 1) as f32 * PAD_GAP + COLUMN_GAP + PAD_COLUMN + COLUMN_GAP + RACK_COLUMN
}

/// The file in `slot` of `device`, if it has one.
fn sample<'a>(cx: &'a Cx, device: DeviceId, slot: &str) -> Option<&'a SampleFile> {
    cx.snap.samples.get(&device)?.iter().find(|file| file.slot == slot)
}

/// A caps heading over a rule, as the laid-out panels have them. Returns
/// where the content under it starts.
pub(super) fn heading(ui: &Ui, cx: &Cx, rect: Rect, title: &str) -> f32 {
    let theme = cx.theme;
    let label = rect.with_max_y(rect.bottom() - 2.0);
    text::paint(ui.painter(), label, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
    paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
    rect.bottom() + 2.0 * theme.metric.gap
}

/// Where a sound dragged over `rect` would land, offered while it is.
/// Returns whether the pointer is there.
fn offer(cx: &mut Cx, rect: Rect, target: DropTarget) -> bool {
    let Some(drag) = &mut cx.state.sound_drag else { return false };
    if !drag.pointer.is_some_and(|pointer| rect.contains(pointer)) {
        return false;
    }
    drag.offer(target);
    true
}

/// The waveform of a sample in a hairline box: its file's outline, with
/// `marks` (fractions of the file) drawn as lines and what lies outside
/// `kept` (the part that plays) shaded.
fn sample_wave(ui: &Ui, cx: &Cx, rect: Rect, path: Option<&str>, accent: Color32, kept: (f32, f32), marks: &[f32]) {
    let theme = cx.theme;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme.bg);
    let inner = rect.shrink(1.0);
    paint::hline(&painter, inner.x_range(), inner.center().y, theme.grid);
    if let Some(WaveState::Ready(wave)) = path.map(|path| cx.sounds.wave(path)) {
        let columns = trill_noir::waveform_columns(&painter, inner.width());
        let found = wave.columns(0.0..wave.seconds(), columns);
        let bands: Vec<(f32, f32)> = found.iter().map(|column| (column.mixed.min, column.mixed.max)).collect();
        let mut mesh = Mesh::default();
        trill_noir::waveform(&mut mesh, inner.shrink2(vec2(0.0, 3.0)), &bands, 1.0, accent, paint::hair(&painter));
        painter.add(Shape::mesh(Arc::new(mesh)));
        let x_of = |fraction: f32| inner.left() + inner.width() * fraction.clamp(0.0, 1.0);
        let (from, to) = (x_of(kept.0.min(kept.1)), x_of(kept.0.max(kept.1)));
        for shaded in [inner.with_max_x(from), inner.with_min_x(to)] {
            if shaded.width() > 0.5 {
                painter.rect_filled(shaded, 0.0, theme.shade);
            }
        }
        for mark in marks {
            paint::vline(&painter, x_of(*mark).round().min(inner.right() - 1.0), inner.y_range(), theme.ink);
        }
    }
    paint::frame(&painter, rect, theme.hairline);
}

// ---- the sampler ---------------------------------------------------------------

/// The sampler's first column: the file it plays.
pub(super) fn sampler_sample(ui: &mut Ui, cx: &mut Cx, body: &Body, column: Rect, panel: Rect) {
    let theme = cx.theme;
    let device = body.device.id;
    let id = body.id.with("sample");
    let file = sample(cx, device, "sample").cloned();
    let path = file.as_ref().map(|file| file.path.clone());
    let (gap, chip) = (theme.metric.gap, theme.metric.chip);
    let mut top = heading(ui, cx, Rect::from_min_size(column.min, vec2(column.width(), HEADING)), "Sample");

    // The name, and the button that takes the file away.
    let name_row = Rect::from_min_size(pos2(column.left(), top), vec2(column.width(), 18.0));
    let painter = ui.painter().clone();
    match &path {
        Some(path) => {
            let clear = Rect::from_center_size(pos2(name_row.right() - 8.0, name_row.center().y), vec2(18.0, 18.0));
            let response =
                trill_noir::bare_icon_button(ui, theme, clear, id.with("clear"), Icon::Close, "Remove the sample");
            cx.hint(&response, "Take the sample out of the sampler");
            if response.clicked() {
                cx.sounds.clear_sample(device, None);
            }
            let room = name_row.with_max_x(clear.left() - 4.0);
            let ink = if body.dimmed { fade(theme.ink, theme.disabled) } else { theme.ink };
            text::paint_name(&painter, room, Align2::LEFT_CENTER, theme.text.body, &file_stem(Path::new(path)), ink);
        }
        None => {
            text::paint(&painter, name_row, Align2::LEFT_CENTER, theme.text.body, "No sample", theme.ink_faint);
        }
    }
    top = name_row.bottom() + gap;

    // The file, with the part that plays lit and the loop marked.
    let wave = Rect::from_min_size(pos2(column.left(), top), vec2(column.width(), WAVE_HEIGHT));
    let percent = |id: &str, fallback: f64| (body.param(id).map_or(fallback, |param| param.value) / 100.0) as f32;
    let kept = (percent("start", 0.0), percent("end", 100.0));
    let looping = body.param("loop_mode").is_some_and(|param| param.value >= 0.5);
    let marks = if looping { vec![percent("loop_start", 0.0), percent("loop_end", 100.0)] } else { Vec::new() };
    let accent = if body.dimmed { fade(body.accent, theme.disabled) } else { body.accent };
    sample_wave(ui, cx, wave, path.as_deref(), accent, kept, &marks);
    let response = ui.interact(wave, id.with("wave"), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Sample waveform"));
    cx.hint(
        &response,
        if path.is_some() {
            "The sample: what plays is lit, the loop is between the marks \u{00B7} click to hear it \u{00B7} drop a sound to replace it"
        } else {
            "Drop a sound here, or use Load"
        },
    );
    if response.clicked()
        && let Some(path) = &path
    {
        let gain = cx.state.browser.sounds.preview_gain_db;
        cx.sounds.preview(Path::new(path), gain);
    }
    if path.is_none() {
        let hint = wave.shrink2(vec2(8.0, 0.0));
        text::paint(&painter, hint, Align2::CENTER_CENTER, theme.text.micro, "Drop a sound here", theme.ink_faint);
    }
    top = wave.bottom() + gap;

    // What is known about the file, or what is wrong with it.
    let line = Rect::from_min_size(pos2(column.left(), top), vec2(column.width(), 16.0));
    match (&file, path.as_deref().map(|path| cx.sounds.wave(path))) {
        (Some(SampleFile { problem: Some(problem), .. }), _) => {
            text::paint(&painter, line, Align2::LEFT_CENTER, theme.text.micro, problem, theme.warn);
        }
        (_, Some(WaveState::Failed(reason))) => {
            text::paint(&painter, line, Align2::LEFT_CENTER, theme.text.micro, &reason, theme.warn);
        }
        (_, Some(WaveState::Ready(wave))) => {
            let words = browser::facts(&wave.facts);
            text::paint(&painter, line, Align2::LEFT_CENTER, theme.text.micro, &words, theme.ink_faint);
        }
        _ => {}
    }
    top = line.bottom() + gap + 2.0;

    let mut x = column.left();
    let mut action = |ui: &Ui, label: &str, enabled: bool| {
        let spec = Chip::action(label).dimmed(!enabled);
        let rect = Rect::from_min_size(pos2(x, top), vec2(spec.width(ui, theme), chip));
        x = rect.right() + gap - 1.0;
        trill_noir::chip(ui, theme, rect, id.with(label), &spec)
    };
    let folder = path.as_deref().and_then(|path| Path::new(path).parent()).map(Path::to_path_buf);
    let response = action(ui, "Load", true);
    cx.hint(&response, "Choose an audio file for the sampler to play");
    if response.clicked() {
        cx.sounds.load_file_dialog(DropTarget::Sampler { device }, folder);
    }
    let response = action(ui, "Reveal", path.is_some());
    cx.hint(&response, "Show the file in the file manager");
    if response.clicked()
        && let Some(path) = &path
    {
        browser::reveal(cx, PathBuf::from(path));
    }
    let response = action(ui, "In browser", path.is_some());
    cx.hint(&response, "Choose this file in the browser");
    if response.clicked()
        && let Some(path) = &path
    {
        browser::choose_file(cx, PathBuf::from(path));
    }

    // A sound dragged anywhere over the panel replaces the sample.
    if offer(cx, panel.intersect(ui.clip_rect()), DropTarget::Sampler { device }) {
        paint::frame(ui.painter(), wave, theme.ink);
    }
}

// ---- the drum rack -------------------------------------------------------------

/// The note of the pad in column `across` of row `down` of the grid. The
/// first pad is at the bottom left, as on the hardware the layout is from.
fn pad_note(across: usize, down: usize) -> u8 {
    let rows = PAD_COUNT / PADS_ACROSS;
    FIRST_NOTE + ((rows - 1 - down) * PADS_ACROSS + across) as u8
}

/// The drum rack's panel body: the pads, the selected pad, the rack.
pub(super) fn drum_rack(ui: &mut Ui, cx: &mut Cx, body: &Body, area: Rect, panel: Rect) {
    let theme = cx.theme;
    let device = body.device.id;
    let chain = cx.state.shown_chain();
    let rows = PAD_COUNT / PADS_ACROSS;
    let grid_width = PADS_ACROSS as f32 * PAD.x + (PADS_ACROSS - 1) as f32 * PAD_GAP;
    let selected = cx.state.selected_pads.get(&device).copied().unwrap_or(FIRST_NOTE);

    // A sound dragged over the panel but not onto a pad goes where its
    // name says; the pads below then offer themselves over this.
    let over_panel = offer(cx, panel.intersect(ui.clip_rect()), DropTarget::Rack { device });

    for down in 0..rows {
        for across in 0..PADS_ACROSS {
            let note = pad_note(across, down);
            let corner = area.min + vec2(across as f32 * (PAD.x + PAD_GAP), down as f32 * (PAD.y + PAD_GAP));
            pad(ui, cx, body, Rect::from_min_size(corner, PAD), note, note == selected, chain);
        }
    }
    if over_panel {
        paint::frame(ui.painter(), panel, theme.ink_faint);
    }

    // The selected pad: its file and its six controls.
    let (bar, gap) = (theme.metric.bar, theme.metric.gap);
    let left = area.left() + grid_width + COLUMN_GAP;
    let title = format!("Pad {}", pad_title(selected));
    let mut top = heading(ui, cx, Rect::from_min_size(pos2(left, area.top()), vec2(PAD_COLUMN, HEADING)), &title);
    let file = sample(cx, device, &format!("pad {selected}")).cloned();
    let wave = Rect::from_min_size(pos2(left, top), vec2(PAD_COLUMN, PAD_WAVE_HEIGHT));
    let start = body.param(&format!("pad{selected}_start")).map_or(0.0, |param| param.value / 100.0) as f32;
    let accent = if body.dimmed { fade(body.accent, theme.disabled) } else { body.accent };
    sample_wave(ui, cx, wave, file.as_ref().map(|file| file.path.as_str()), accent, (start, 1.0), &[]);
    if file.is_none() {
        let hint = wave.shrink2(vec2(6.0, 0.0));
        text::paint(ui.painter(), hint, Align2::CENTER_CENTER, theme.text.micro, "Empty pad", theme.ink_faint);
    }
    top = wave.bottom() + gap;
    for (suffix, label) in [
        ("level", "Level"),
        ("pan", "Pan"),
        ("tune", "Tune"),
        ("start", "Start"),
        ("decay", "Decay"),
        ("choke", "Choke"),
    ] {
        let rect = Rect::from_min_size(pos2(left, top), vec2(PAD_COLUMN, bar));
        if let Some(param) = body.param(&format!("pad{selected}_{suffix}")) {
            devices::param_bar(ui, cx, body, param, label, Format::Auto, rect);
        }
        top += bar + gap;
    }

    // The rack as a whole.
    let left = left + PAD_COLUMN + COLUMN_GAP;
    let mut top = heading(ui, cx, Rect::from_min_size(pos2(left, area.top()), vec2(RACK_COLUMN, HEADING)), "Rack");
    for (id, label) in [("volume", "Volume"), ("velocity_sens", "Velocity")] {
        let rect = Rect::from_min_size(pos2(left, top), vec2(RACK_COLUMN, bar));
        if let Some(param) = body.param(id) {
            devices::param_bar(ui, cx, body, param, label, Format::Auto, rect);
        }
        top += bar + gap;
    }
    let loaded = cx.snap.samples.get(&device).map_or(0, Vec::len);
    let summary = Rect::from_min_size(pos2(left, top + 4.0), vec2(RACK_COLUMN, 16.0));
    let words = match loaded {
        0 => "No pads loaded yet".to_owned(),
        count => format!("{count} of {PAD_COUNT} pads loaded"),
    };
    text::paint(ui.painter(), summary, Align2::LEFT_CENTER, theme.text.micro, &words, theme.ink_faint);
}

/// One pad: the note it answers to, the file on it, and what can be done
/// with it. A click selects it and plays it.
fn pad(ui: &mut Ui, cx: &mut Cx, body: &Body, rect: Rect, note: u8, selected: bool, chain: ChainRef) {
    let theme = cx.theme;
    let device = body.device.id;
    let id = body.id.with(("pad", note));
    let file = sample(cx, device, &format!("pad {note}")).cloned();
    let name = file.as_ref().map(|file| file_stem(Path::new(&file.path)));
    let response = ui.interact(rect, id, Sense::click());
    let label = match &name {
        Some(name) => format!("Pad {}: {name}", pad_title(note)),
        None => format!("Pad {}: empty", pad_title(note)),
    };
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &label));
    let dropping = offer(cx, rect.intersect(ui.clip_rect()), DropTarget::Pad { device, note });

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let lit = response.hovered() || dropping;
        painter.rect_filled(
            rect,
            0.0,
            if selected {
                theme.selected
            } else if lit {
                theme.hover
            } else {
                theme.well
            },
        );
        paint::frame(&painter, rect, if dropping || selected { theme.ink } else { theme.hairline });
        let quiet = theme.quiet_on(if selected { theme.raised } else { theme.panel });
        let inner = rect.shrink2(vec2(6.0, 4.0));
        // The note and what the drum map calls it, along the top.
        let caption = Rect::from_min_size(inner.min, vec2(inner.width(), 14.0));
        let number = trill_noir::TextStyle { size: 10.0, ..theme.text.value };
        let used = text::paint(&painter, caption, Align2::LEFT_CENTER, number, &note.to_string(), quiet);
        let drum = PAD_NAMES[usize::from(note - FIRST_NOTE)];
        text::paint(
            &painter,
            caption.with_min_x(caption.left() + used + 5.0),
            Align2::LEFT_CENTER,
            theme.text.micro,
            drum,
            quiet,
        );
        // The file, or that there is none.
        let line = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 18.0), inner.max);
        let failed = file.as_ref().is_some_and(|file| file.problem.is_some());
        // An empty pad says so by having nothing on it.
        if let Some(name) = &name {
            let style = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
            let ink = match (failed, body.dimmed) {
                (true, _) => theme.warn,
                (false, true) => fade(theme.ink, theme.disabled),
                (false, false) => theme.ink,
            };
            text::paint_name(&painter, line, Align2::LEFT_CENTER, style, name, ink);
            // A loaded pad carries the track's color.
            let accent = if body.dimmed { fade(body.accent, theme.disabled) } else { body.accent };
            let strip = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), rect.max);
            painter.rect_filled(strip, 0.0, if failed { theme.warn } else { accent });
        }
    }

    let about = match (&file, dropping) {
        (_, true) => String::new(),
        (Some(SampleFile { problem: Some(problem), .. }), _) => problem.clone(),
        (Some(file), _) => format!(
            "{} \u{00B7} click to hear it \u{00B7} drop a sound to replace it \u{00B7} right-click for more",
            file.path
        ),
        (None, _) => "An empty pad \u{00B7} drop a sound on it, or right-click to load a file".to_owned(),
    };
    if !about.is_empty() {
        cx.hint(&response, about);
    }
    // Taking the file away, without a trip through the menu. Shown while
    // the pointer is anywhere on the pad, the button itself included.
    if file.is_some() && ui.rect_contains_pointer(rect) {
        let clear = Rect::from_center_size(pos2(rect.right() - 10.0, rect.top() + 10.0), vec2(16.0, 16.0));
        let name = format!("Clear pad {note}");
        let button = trill_noir::bare_icon_button(ui, theme, clear, id.with("clear"), Icon::Close, &name);
        cx.hint(&button, "Take the sound off this pad");
        if button.clicked() {
            cx.sounds.clear_sample(device, Some(note));
            return;
        }
    }
    if response.clicked() {
        cx.state.selected_pads.insert(device, note);
        // Heard through the track, so with its effects and at its level.
        if let ChainRef::Track(track) = chain {
            cx.audition(track, note, 100, Some(Instant::now() + AUDITION_LENGTH));
        }
    }
    if response.secondary_clicked() {
        cx.state.selected_pads.insert(device, note);
    }
    let path = file.map(|file| PathBuf::from(file.path));
    response.context_menu(|ui| {
        ui.set_min_width(190.0);
        let folder = path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        if MenuItem::new("Load a file\u{2026}").show(ui, theme).clicked() {
            cx.sounds.load_file_dialog(DropTarget::Pad { device, note }, folder);
        }
        if MenuItem::new("Clear the pad").enabled(path.is_some()).show(ui, theme).clicked() {
            cx.sounds.clear_sample(device, Some(note));
        }
        trill_noir::menu_separator(ui, theme);
        if MenuItem::new("Choose in the browser").enabled(path.is_some()).show(ui, theme).clicked()
            && let Some(path) = &path
        {
            browser::choose_file(cx, path.clone());
        }
        if MenuItem::new("Show in the file manager").enabled(path.is_some()).show(ui, theme).clicked()
            && let Some(path) = &path
        {
            browser::reveal(cx, path.clone());
        }
        if MenuItem::new("Copy path").enabled(path.is_some()).show(ui, theme).clicked()
            && let Some(path) = &path
        {
            cx.state.copied_text = Some(path.display().to_string());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pads_run_from_the_bottom_left_like_the_hardware() {
        assert_eq!(pad_note(0, 3), 36, "the kick is at the bottom left");
        assert_eq!(pad_note(3, 3), 39);
        assert_eq!(pad_note(0, 2), 40);
        assert_eq!(pad_note(0, 0), 48);
        assert_eq!(pad_note(3, 0), 51, "the last pad is at the top right");
        let mut notes: Vec<u8> = (0..4).flat_map(|down| (0..4).map(move |across| pad_note(across, down))).collect();
        notes.sort_unstable();
        let pads: Vec<u8> = trill_dsp::drumrack::PAD_NOTES.collect();
        assert_eq!(notes, pads, "every pad once");
    }

    #[test]
    fn pads_are_called_by_note_and_drum() {
        assert_eq!(pad_title(36), "36 Kick");
        assert_eq!(pad_title(42), "42 Closed Hat");
        assert_eq!(pad_title(51), "51 Ride");
        assert_eq!(pad_title(60), "60", "not a pad: just the note");
        assert_eq!(pad_title(12), "12");
        assert!(plays_samples(SAMPLER) && plays_samples(DRUM_RACK) && !plays_samples("builtin:polysynth"));
    }

    #[test]
    fn the_rack_fits_the_shortest_panel() {
        let rows = (PAD_COUNT / PADS_ACROSS) as f32;
        let pads = rows * PAD.y + (rows - 1.0) * PAD_GAP;
        assert!(pads <= devices::MIN_BODY_HEIGHT, "the pads are {pads} tall");
        let metric = trill_noir::Theme::noir().metric;
        let column = HEADING + 2.0 * metric.gap + PAD_WAVE_HEIGHT + metric.gap + 6.0 * (metric.bar + metric.gap);
        assert!(column <= devices::MIN_BODY_HEIGHT, "the pad's controls are {column} tall");
        let sample = HEADING + 2.0 * metric.gap + 18.0 + WAVE_HEIGHT + 16.0 + metric.chip + 4.0 * metric.gap + 2.0;
        assert!(sample <= devices::MIN_BODY_HEIGHT, "the sampler's file column is {sample} tall");
    }
}
