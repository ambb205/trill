//! The Clip tab for an audio clip: where a MIDI clip shows its notes, an
//! audio clip shows its file.
//!
//! Along the top are the controls that say how the recording follows the
//! song: the warp mode, the tempo it was recorded at and its transposition.
//! Below them is the waveform of the file, with the stretch the clip plays
//! lit and its fades drawn over it; the wheel pans and Ctrl+wheel zooms,
//! as everywhere. The sidebar's part (gain, offset, fades) is
//! [`sidebar`]. A clip whose file is missing says so here and offers to be
//! pointed at it again.

use super::piano_roll::Stack;
use super::sounds::{WaveData, WaveState, file_name};
use super::state::Cx;
use super::{arrangement, browser, colors, units};
use eframe::egui::{self, Align2, Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use std::path::Path;
use std::sync::Arc;
use trill_core::project::{AudioContent, ClipContent, MAX_TEMPO, MAX_TRANSPOSE, MIN_TEMPO, WarpMode};
use trill_core::{Clip, ClipId, EditError, Project, Track};
use trill_noir::{Bar, Chip, NumberField, Theme, fade, paint, text};

/// Height of a row of the warp controls.
pub(super) const TOOLBAR: f32 = 30.0;
/// The width below which the warp controls take two rows: the modes on
/// the first, the source tempo and the transposition on the second.
const TOOLBAR_ONE_ROW: f32 = 760.0;
/// Height of the ruler above the waveform.
const RULER: f32 = 18.0;
/// Height of the line under the waveform that names the file.
const FILE_LINE: f32 = 22.0;
/// The range of the gain bar, in decibels.
const GAIN: (f32, f32) = (-36.0, 24.0);
/// The shortest stretch of a file the view zooms in to, in seconds.
const CLOSEST: f64 = 0.004;
/// Points along a fade's curve.
const FADE_STEPS: usize = 24;

/// Picks the range of one channel, or of the mix, out of a column.
type Channel = fn(&trill_media::PeakColumn) -> (f32, f32);

const WARP_MODES: [(WarpMode, &str, &str); 4] = [
    (WarpMode::Off, "Off", "Plays at the speed and pitch it was recorded at, whatever the song\u{2019}s tempo"),
    (WarpMode::Repitch, "Repitch", "Follows the tempo by playing faster or slower, like a record: the pitch moves too"),
    (WarpMode::Beats, "Beats", "Follows the tempo and keeps the pitch, keeping hits sharp: for drums"),
    (
        WarpMode::Tonal,
        "Tonal",
        "Follows the tempo and keeps the pitch, keeping sustained sound smooth: for chords, bass, vocals",
    ),
];

/// Changes the audio of `clip`, as one undo step.
fn edit_audio(cx: &mut Cx, clip: ClipId, change: impl FnOnce(&mut AudioContent)) {
    cx.edit(|project| {
        match &mut project.clip_mut(clip)?.content {
            ClipContent::Audio(audio) => change(audio),
            ClipContent::Midi(_) => {}
        }
        Ok(())
    });
}

/// Sets how `clip` follows the tempo, and resizes it so that it goes on
/// playing the stretch of the recording it played before: a loop that is
/// warped becomes as many beats long as it has beats in it.
fn rewarp(
    project: &mut Project,
    clip: ClipId,
    warp: WarpMode,
    source_tempo: Option<f64>,
    transpose: f64,
) -> Result<(), EditError> {
    let before = project.clip(clip)?.clone();
    // At the tempo the song has where the clip starts.
    let tempo = project.tempo_map().tempo_at(before.start);
    let Some(audio) = before.audio() else { return Ok(()) };
    let covered = before.length * audio.seconds_per_beat(tempo);
    project.set_clip_warp(clip, warp, source_tempo, transpose)?;
    let per_beat = project.clip(clip)?.audio().map_or(1.0, |audio| audio.seconds_per_beat(tempo));
    let length = (covered / per_beat).max(1.0 / 64.0);
    // The fades are lengths on the timeline; they shrink with a clip that
    // has become too short for them.
    let room = length / (audio.fade_in + audio.fade_out).max(length);
    project.set_clip_fades(clip, audio.fade_in * room, audio.fade_out * room)?;
    project.set_clip_span(clip, before.start, length)
}

/// The audio clip's rows of the Clip tab's sidebar: gain, where in the file
/// it starts, and its fades.
pub(super) fn sidebar(ui: &mut Ui, cx: &mut Cx, clip: &Clip, audio: &AudioContent, stack: &mut Stack) {
    let theme = cx.theme;
    let bar = theme.metric.bar;
    let id = ui.scope_id().with("audio-sidebar");
    let clip_id = clip.id;
    stack.heading(ui, theme, "Audio", false);

    let (low, high) = GAIN;
    let shown = units::format_db(audio.gain_db);
    let spec = Bar::new("Gain", &shown, (audio.gain_db - low) / (high - low))
        .default((0.0 - low) / (high - low))
        .bipolar((0.0 - low) / (high - low));
    let output = trill_noir::bar(ui, theme, stack.row(bar), id.with("gain"), &spec);
    cx.hint(
        &output.response,
        format!("Gain of this clip \u{00B7} {shown} \u{00B7} drag to set, double-click for 0 dB"),
    );
    if output.pressed {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac {
        let gain = if output.reset { 0.0 } else { ((low + position * (high - low)) * 10.0).round() / 10.0 };
        if gain != audio.gain_db {
            edit_audio(cx, clip_id, |audio| audio.gain_db = gain);
        }
    }

    // Where in the file the clip starts. It cannot start past the file's
    // end, when that is known.
    let file_seconds = cx.sounds.known_wave(&audio.path).map(|wave| wave.seconds());
    let latest = file_seconds.map_or(86_400.0, |seconds| (seconds - 0.001).max(0.0));
    let shown = format!("{:.3}", audio.offset_seconds);
    let field = NumberField {
        label: "Offset",
        text: &shown,
        unit: "s",
        value: audio.offset_seconds,
        range: 0.0..=latest,
        per_point: 0.005,
        style: theme.text.value,
        name: "Offset into the file",
    };
    let output = trill_noir::number_field(ui, theme, stack.row(bar), id.with("offset"), &field);
    cx.hint(&output.response, "Where in the file the clip starts playing \u{00B7} drag, or double-click to type");
    if output.response.drag_started() {
        cx.begin_gesture();
    }
    if let Some(offset) = output.value.filter(|offset| *offset != audio.offset_seconds) {
        edit_audio(cx, clip_id, |audio| audio.offset_seconds = offset);
    }

    for (label, name, fade_in) in [("Fade in", "Fade in", true), ("Fade out", "Fade out", false)] {
        let (value, other) = if fade_in { (audio.fade_in, audio.fade_out) } else { (audio.fade_out, audio.fade_in) };
        let shown = format!("{value:.2}");
        let field = NumberField {
            label,
            text: &shown,
            unit: "beats",
            value,
            range: 0.0..=(clip.length - other).max(0.0),
            per_point: 0.02,
            style: theme.text.value,
            name,
        };
        let output = trill_noir::number_field(ui, theme, stack.row(bar), id.with(name), &field);
        cx.hint(&output.response, "Length of the fade, in beats \u{00B7} drag, or double-click to type");
        if output.response.drag_started() {
            cx.begin_gesture();
        }
        if let Some(length) = output.value.filter(|length| *length != value) {
            let (fade_in, fade_out) = if fade_in { (length, other) } else { (other, length) };
            cx.edit(|project| project.set_clip_fades(clip_id, fade_in, fade_out));
        }
    }
}

/// The part of the Clip tab right of the sidebar.
pub(super) fn show(ui: &mut Ui, cx: &mut Cx, track: &Track, clip: &Clip, audio: &AudioContent, area: Rect) {
    let theme = cx.theme;
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme.bg);
    let rows = if area.width() < TOOLBAR_ONE_ROW { 2.0 } else { 1.0 };
    let toolbar_rect = Rect::from_min_size(area.min, vec2(area.width(), rows * TOOLBAR - (rows - 1.0) * 6.0));
    painter.rect_filled(toolbar_rect, 0.0, theme.panel);
    paint::hline_above(&painter, toolbar_rect.x_range(), toolbar_rect.bottom(), theme.hairline);
    let state = cx.sounds.wave(&audio.path);
    toolbar(ui, cx, clip, audio, &state, toolbar_rect);

    let file_line = Rect::from_min_max(pos2(area.left(), area.bottom() - FILE_LINE), area.max);
    painter.rect_filled(file_line, 0.0, theme.panel);
    paint::hline(&painter, file_line.x_range(), file_line.top(), theme.hairline);
    file_row(ui, cx, clip, audio, &state, file_line);

    let view = Rect::from_min_max(pos2(area.left(), toolbar_rect.bottom()), pos2(area.right(), file_line.top()));
    match &state {
        WaveState::Ready(wave) => {
            let color = colors::track_color(theme, track.color);
            waveform(ui, cx, clip, audio, wave, view, color);
        }
        WaveState::Loading => {
            paint::hline(&painter, view.x_range(), view.center().y, theme.ghost);
            let room = Rect::from_center_size(view.center() - vec2(0.0, 14.0), vec2(view.width() - 32.0, 18.0));
            text::paint(
                &painter,
                room,
                Align2::CENTER_CENTER,
                theme.text.caps,
                "Reading the file\u{2026}",
                theme.ink_faint,
            );
        }
        WaveState::Failed(reason) => missing(ui, cx, clip, audio, reason, view),
    }
}

/// Warp mode, source tempo, transposition, and what the engine is doing.
fn toolbar(ui: &mut Ui, cx: &mut Cx, clip: &Clip, audio: &AudioContent, state: &WaveState, bar: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with("audio-toolbar");
    let clip_id = clip.id;
    let chip = theme.metric.chip;
    let two_rows = bar.height() > TOOLBAR;
    let mut top = bar.top() + ((TOOLBAR - chip) / 2.0).round();
    let painter = ui.painter().clone();
    let mut x = bar.left() + 12.0;
    let label = |x: &mut f32, top: f32, word: &str| {
        let room = Rect::from_min_max(pos2(*x, top), pos2(bar.right(), top + chip));
        *x += text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, word, theme.ink_faint) + 6.0;
    };

    label(&mut x, top, "Warp");
    let labels: Vec<&str> = WARP_MODES.iter().map(|(_, label, _)| *label).collect();
    let current = WARP_MODES.iter().position(|(mode, ..)| *mode == audio.warp);
    let width = trill_noir::chip_row_width(ui, theme, &labels);
    let (clicked, _) =
        trill_noir::chip_row(ui, theme, pos2(x, top), width + 1.0, id.with("warp"), &labels, current, false);
    let row = Rect::from_min_size(pos2(x, top), vec2(width, chip));
    if let Some(pointer) = ui.input(|input| input.pointer.hover_pos()).filter(|pointer| row.contains(*pointer)) {
        // Which chip the pointer is on, for the hint.
        let mut left = row.left();
        for (_, label, about) in WARP_MODES {
            let right = left + Chip::new(label, false).width(ui, theme) + theme.metric.gap - 1.0;
            if pointer.x < right {
                cx.state.hint.set(about);
                break;
            }
            left = right;
        }
    }
    if let Some(index) = clicked {
        let mode = WARP_MODES[index].0;
        // To follow the tempo a clip needs the tempo it was recorded at:
        // the one it has, the one its name states, or, failing both, the
        // song's, which changes nothing until it is corrected.
        let named = state.ready().and_then(|wave| wave.facts.tempo);
        let assumed = audio.source_tempo.or(named).is_none() && mode != WarpMode::Off;
        let source = audio.source_tempo.or(named).or(assumed.then_some(cx.snap.project.tempo));
        let transpose = audio.transpose;
        if cx.edit(|project| rewarp(project, clip_id, mode, source, transpose)).is_some() && assumed {
            cx.state.toasts.info(format!(
                "{} does not say its tempo, so the song\u{2019}s was assumed. Set Source to the tempo it was recorded at.",
                clip.name
            ));
        }
    }
    x += width + 14.0;
    // In a narrow tab what is going on with the clip is said here, after
    // the modes, and the rest moves to a row of its own.
    let status_room = Rect::from_min_max(pos2(x, top), pos2(bar.right() - 12.0, top + chip));
    if two_rows {
        x = bar.left() + 12.0;
        top += TOOLBAR - 6.0;
    }

    // The tempo of the recording: what a warped clip follows the song with.
    let warped = audio.warp != WarpMode::Off;
    let source = audio.source_tempo;
    let shown = source.map_or_else(|| "\u{2013}".to_owned(), |tempo| format!("{tempo:.2}"));
    let field = NumberField {
        label: "Source",
        text: &shown,
        unit: "BPM",
        value: source.unwrap_or(cx.snap.project.tempo),
        range: MIN_TEMPO..=MAX_TEMPO,
        per_point: 0.1,
        style: theme.text.value,
        name: "Source tempo",
    };
    let rect = Rect::from_min_size(pos2(x, top), vec2(162.0, chip));
    let output = trill_noir::number_field(ui, theme, rect, id.with("source"), &field);
    cx.hint(
        &output.response,
        "The tempo the file was recorded at: what a warped clip follows the song with \u{00B7} drag, or double-click to type",
    );
    if output.response.drag_started() {
        cx.begin_gesture();
    }
    let (mode, transpose) = (audio.warp, audio.transpose);
    let set_source = |cx: &mut Cx, tempo: f64| {
        let tempo = ((tempo * 100.0).round() / 100.0).clamp(MIN_TEMPO, MAX_TEMPO);
        if source != Some(tempo) {
            cx.edit(|project| rewarp(project, clip_id, mode, Some(tempo), transpose));
        }
    };
    if let Some(tempo) = output.value {
        set_source(cx, tempo);
    }
    x = rect.right() + theme.metric.gap;
    for (label, factor, about) in [
        ("\u{00F7}2", 0.5, "Half the source tempo: for a file that plays twice too fast"),
        ("\u{00D7}2", 2.0, "Double the source tempo: for a file that plays at half speed"),
    ] {
        let spec = Chip::action(label).dimmed(source.is_none());
        let rect = Rect::from_min_size(pos2(x, top), vec2(spec.width(ui, theme), chip));
        let response = trill_noir::chip(ui, theme, rect, id.with(label), &spec);
        cx.hint(&response, about);
        if response.clicked()
            && let Some(tempo) = source
        {
            set_source(cx, tempo * factor);
        }
        x = rect.right() + theme.metric.gap - 1.0;
    }
    x += 12.0;

    // Transposing needs a mode that can change pitch without speed.
    let can_transpose = audio.warp.keeps_pitch();
    let shown = trill_noir::format::fmt_val_signed(audio.transpose, 0, "st");
    let range = MAX_TRANSPOSE as f32;
    let spec = Bar::new("Transpose", &shown, (audio.transpose as f32 + range) / (2.0 * range))
        .default(0.5)
        .bipolar(0.5)
        .dimmed(!can_transpose);
    let rect = Rect::from_min_size(pos2(x, top), vec2(150.0, chip));
    let output = trill_noir::bar(ui, theme, rect, id.with("transpose"), &spec);
    cx.hint(
        &output.response,
        if can_transpose {
            "Pitch shift in semitones, without changing the speed \u{00B7} drag to set, double-click for none"
        } else {
            "Transposing needs Beats or Tonal warping: the other modes tie pitch to speed"
        },
    );
    if output.pressed && can_transpose {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac.filter(|_| can_transpose) {
        let semitones = if output.reset { 0.0 } else { f64::from(position * 2.0 * range - range).round() };
        if semitones != audio.transpose {
            cx.edit(|project| project.set_clip_warp(clip_id, mode, source, semitones));
        }
    }
    x = rect.right() + 16.0;

    // Round and round: what the clip plays now becomes what repeats when its end is drawn out.
    let looping = audio.looped.is_some();
    let spec = Chip::new("Loop", looping).named("Loop the clip");
    let rect = Rect::from_min_size(pos2(x, top), vec2(spec.width(ui, theme), chip));
    let response = trill_noir::chip(ui, theme, rect, id.with("loop"), &spec);
    cx.hint(
        &response,
        "Repeat the stretch the clip plays now: drawing its end out in the arrangement then plays it round",
    );
    if response.clicked() {
        cx.edit(|project| project.loop_clip(clip_id, !looping));
    }
    x = rect.right() + 16.0;

    // What is going on with the clip, at the right.
    let room =
        if two_rows { status_room } else { Rect::from_min_max(pos2(x, top), pos2(bar.right() - 12.0, top + chip)) };
    let problem = cx.snap.clip_problems.get(&clip.id);
    let stretched = warped && audio.source_tempo.is_some();
    if cx.snap.arriving.contains(&clip.id) {
        // On its way from someone else who has the song: not missing.
        status(&painter, theme, room, theme.info, "Its audio is on its way");
    } else if matches!(state, WaveState::Failed(_)) {
        status(&painter, theme, room, theme.warn, "File missing");
    } else if problem.is_some_and(|reason| reason.contains("stretch")) {
        status(&painter, theme, room, theme.warn, "Could not be stretched: plays repitched");
    } else if problem.is_some() {
        status(&painter, theme, room, theme.warn, "The file cannot be played");
    } else if stretched && cx.live.preparing > 0 {
        status(&painter, theme, room, theme.info, "Preparing the stretched audio\u{2026}");
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
    } else if warped && audio.source_tempo.is_none() {
        status(&painter, theme, room, theme.warn, "No source tempo: plays at its recorded speed");
    }
}

/// A dot and a few words, right-aligned in `room`.
fn status(painter: &egui::Painter, theme: &Theme, room: Rect, color: Color32, words: &str) {
    if room.width() < 60.0 {
        return;
    }
    let line = text::layout(painter, theme.text.micro, words, color, room.width() - theme.metric.dot - 6.0);
    let left = room.right() - line.width() - theme.metric.dot - 5.0;
    paint::dot(painter, pos2(left, room.center().y), theme.metric.dot, color);
    text::place(painter, room, Align2::RIGHT_CENTER, line);
}

/// The name of the file, what is known about it, and what can be done
/// with it.
fn file_row(ui: &mut Ui, cx: &mut Cx, clip: &Clip, audio: &AudioContent, state: &WaveState, row: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with("audio-file");
    let painter = ui.painter().clone();
    let chip = theme.metric.chip;
    let top = (row.center().y - chip / 2.0).round();
    let mut right = row.right() - 10.0;
    let mut action = |ui: &Ui, label: &str| {
        let spec = Chip::action(label);
        let width = spec.width(ui, theme);
        let rect = Rect::from_min_size(pos2(right - width, top), vec2(width, chip));
        right = rect.left() - theme.metric.gap + 1.0;
        trill_noir::chip(ui, theme, rect, id.with(label), &spec)
    };
    let path = Path::new(&audio.path).to_path_buf();
    let response = action(ui, "Locate");
    cx.hint(&response, "Point the clip at another file: for one that was moved or renamed");
    if response.clicked() {
        cx.sounds.relink_dialog(clip.id, path.parent().map(Path::to_path_buf));
    }
    let response = action(ui, "Reveal");
    cx.hint(&response, "Show the file in the file manager");
    if response.clicked() {
        browser::reveal(cx, path.clone());
    }
    let response = action(ui, "In browser");
    cx.hint(&response, "Choose this file in the browser, to hear it or drag it onto a pad or a sampler");
    if response.clicked() {
        browser::choose_file(cx, path.clone());
    }
    let response = action(ui, "Copy path");
    cx.hint(&response, audio.path.clone());
    if response.clicked() {
        cx.state.copied_text = Some(audio.path.clone());
    }

    let content = Rect::from_min_max(pos2(row.left() + 12.0, row.top()), pos2(right - 8.0, row.bottom()));
    // A file name keeps its case: it is a name, not a label.
    let name_style = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
    let name_room = content.with_max_x(content.left() + content.width() * 0.6);
    let used = text::paint_name(&painter, name_room, Align2::LEFT_CENTER, name_style, &file_name(&path), theme.ink_dim);
    let rest = content.with_min_x(content.left() + used + 12.0);
    let (words, color) = match state {
        WaveState::Ready(wave) => (browser::facts(&wave.facts), theme.ink_faint),
        WaveState::Loading => ("Reading\u{2026}".to_owned(), theme.ink_faint),
        WaveState::Failed(_) => ("Missing".to_owned(), theme.warn),
    };
    if rest.width() > 40.0 {
        text::paint(&painter, rest, Align2::LEFT_CENTER, theme.text.micro, &words, color);
    }
}

/// What the tab shows for a clip whose file cannot be read.
fn missing(ui: &mut Ui, cx: &mut Cx, clip: &Clip, audio: &AudioContent, reason: &str, view: Rect) {
    let theme = cx.theme;
    let painter = ui.painter_at(view);
    let middle = view.center().y.round();
    let line = |offset: f32| {
        Rect::from_min_max(pos2(view.left() + 16.0, middle + offset), pos2(view.right() - 16.0, middle + offset + 18.0))
    };
    text::paint(&painter, line(-34.0), Align2::CENTER_CENTER, theme.text.caps, "This clip is silent", theme.warn);
    let quiet = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
    // What was found for its file, if the file is one of the song's
    // missing ones and something was: said in place of the bare reason.
    let found = cx.sounds.missing.found.clone();
    let file = found.files.iter().find(|file| file.path == audio.path);
    let candidate = file.and_then(|file| file.found());
    match candidate {
        Some(candidate) => {
            let says = format!(
                "The file is not on disk. {} {}",
                match candidate.confidence {
                    trill_runtime::Confidence::Sure => "It was found in",
                    trill_runtime::Confidence::Likely => "It is likely in",
                    trill_runtime::Confidence::Unsure => "A file of its name is in",
                },
                trill_core::files::folder_of(&candidate.path)
            );
            text::paint_name(&painter, line(-14.0), Align2::CENTER_CENTER, quiet, &says, theme.ink_dim);
        }
        None => {
            text::paint(&painter, line(-14.0), Align2::CENTER_CENTER, quiet, reason, theme.ink_dim);
        }
    }
    if view.height() <= 90.0 {
        return;
    }
    // The buttons, side by side in the middle: what was found, locating
    // the file by hand, and the list of everything that is missing.
    let use_found = candidate.map(|_| "Use it");
    let others = found.files.len() > 1;
    let labels = [use_found, Some("Locate the file\u{2026}"), others.then_some("All missing files\u{2026}")];
    let gap = theme.metric.gap;
    let widths: Vec<f32> = labels.iter().flatten().map(|label| trill_noir::button_width(ui, theme, label)).collect();
    let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    let row = Rect::from_center_size(pos2(view.center().x, middle + 22.0), vec2(total + 2.0, theme.metric.control));
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    super::widgets::place(ui, row, "locate", layout, |ui| {
        ui.spacing_mut().item_spacing = vec2(gap, 0.0);
        if let (Some(label), Some(candidate), Some(file)) = (use_found, candidate, file) {
            let response = trill_noir::button(ui, theme, label, trill_noir::Variant::Primary);
            cx.hint(&response, format!("{}: {}", candidate.path, candidate.why));
            if response.clicked() {
                cx.sounds.relink_file(file.path.clone(), candidate.path.clone().into());
            }
        }
        let variant = if use_found.is_some() { trill_noir::Variant::Default } else { trill_noir::Variant::Primary };
        let response = trill_noir::button(ui, theme, "Locate the file\u{2026}", variant);
        cx.hint(&response, "Choose where the file is now; the clip keeps its place, length and settings");
        if response.clicked() {
            cx.sounds.relink_dialog(clip.id, Path::new(&audio.path).parent().map(Path::to_path_buf));
        }
        if others {
            let response = trill_noir::button(ui, theme, "All missing files\u{2026}", trill_noir::Variant::Default);
            cx.hint(&response, "The list of every file this song plays that is not on disk, and what was found");
            if response.clicked() {
                super::missing::open(cx);
            }
        }
    });
}

/// The stretch of the file, in seconds, the clip plays at `tempo`.
fn played(clip: &Clip, audio: &AudioContent, tempo: f64) -> (f64, f64) {
    // A clip that plays round plays its loop, however long it is drawn out.
    match audio.round() {
        Some(round) => (round.start_seconds, round.end_seconds()),
        None => (audio.offset_seconds, audio.offset_seconds + clip.length * audio.seconds_per_beat(tempo)),
    }
}

/// The seconds of the file to show for a clip that has just been opened:
/// the stretch it plays, with some of what surrounds it.
fn framed(clip: &Clip, audio: &AudioContent, tempo: f64, file_seconds: f64) -> (f64, f64) {
    let (start, end) = played(clip, audio, tempo);
    let margin = ((end - start) * 0.06).max(0.01);
    let from = (start - margin).max(0.0);
    let to = (end + margin).min(file_seconds.max(end)).max(from + CLOSEST);
    (from, to)
}

/// `"0:01.250"`, or `"1.250"` under a minute.
fn format_time(seconds: f64, step: f64) -> String {
    let decimals = if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else if step >= 0.01 {
        2
    } else {
        3
    };
    let seconds = seconds.max(0.0);
    if seconds >= 60.0 {
        let minutes = (seconds / 60.0).floor();
        format!(
            "{minutes:.0}:{:0width$.decimals$}",
            seconds - minutes * 60.0,
            width = if decimals == 0 { 2 } else { decimals + 3 }
        )
    } else {
        format!("{seconds:.decimals$}")
    }
}

/// A step between ruler marks, in seconds, that puts them at least
/// `min_pixels` apart: 1, 2 or 5 times a power of ten.
fn ruler_step(seconds_per_pixel: f64, min_pixels: f64) -> f64 {
    let wanted = (seconds_per_pixel * min_pixels).max(1e-6);
    let power = 10f64.powf(wanted.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|factor| factor * power).find(|step| *step >= wanted).unwrap_or(power * 10.0)
}

/// The waveform of the file, the stretch the clip plays lit, with a ruler
/// in seconds above it.
fn waveform(
    ui: &mut Ui,
    cx: &mut Cx,
    clip: &Clip,
    audio: &AudioContent,
    wave: &Arc<WaveData>,
    view: Rect,
    color: Color32,
) {
    let theme = cx.theme;
    if view.width() < 8.0 || view.height() < RULER + 8.0 {
        return;
    }
    // At the tempo the song has where the clip starts.
    let tempo = cx.snap.project.tempo_map().tempo_at(clip.start);
    let file_seconds = wave.seconds();
    let (clip_start, clip_end) = played(clip, audio, tempo);
    // What is in view: kept while the same clip is shown, framed afresh
    // when another is.
    let kept = cx.state.audio_view.filter(|(shown, ..)| *shown == clip.id);
    let (mut from, mut to) = kept.map_or_else(|| framed(clip, audio, tempo, file_seconds), |(_, from, to)| (from, to));

    let ruler = Rect::from_min_size(view.min, vec2(view.width(), RULER));
    let body = Rect::from_min_max(pos2(view.left(), ruler.bottom()), view.max);
    let response = ui.interact(view, ui.scope_id().with("audio-wave"), Sense::click_and_drag());
    cx.hint(
        &response,
        "The file, with what the clip plays lit \u{00B7} scroll to move, Ctrl+scroll to zoom, double-click to frame the clip",
    );
    if let Some(pointer) = response.hover_pos() {
        let (scroll, zoom) = ui.input(|input| (input.smooth_scroll_delta, input.zoom_delta()));
        let span = to - from;
        if zoom != 1.0 {
            let anchor = from + f64::from((pointer.x - view.left()) / view.width()) * span;
            let longest = (file_seconds.max(clip_end) * 1.1).max(CLOSEST);
            let wanted = (span / f64::from(zoom)).clamp(CLOSEST, longest);
            from = anchor - (anchor - from) * wanted / span;
            to = from + wanted;
        }
        if scroll != egui::Vec2::ZERO {
            let shift = -f64::from(scroll.x + scroll.y) / f64::from(view.width()) * (to - from);
            from += shift;
            to += shift;
        }
    }
    if response.dragged() {
        let shift = -f64::from(response.drag_delta().x) / f64::from(view.width()) * (to - from);
        from += shift;
        to += shift;
    }
    if super::widgets::double_clicked(&response) {
        (from, to) = framed(clip, audio, tempo, file_seconds);
    }
    // Never further out than the file and the clip reach.
    let reach = file_seconds.max(clip_end);
    let span = (to - from).min(reach * 1.1).max(CLOSEST);
    from = from.clamp(-0.05 * span, (reach - span * 0.95).max(-0.05 * span));
    to = from + span;
    cx.state.audio_view = Some((clip.id, from, to));

    let painter = ui.painter_at(view);
    let x_of = |second: f64| view.left() + ((second - from) / span) as f32 * view.width();
    painter.rect_filled(ruler, 0.0, theme.panel);
    paint::hline_above(&painter, ruler.x_range(), ruler.bottom(), theme.hairline);

    // What the clip plays is lit; the rest of the file steps back.
    let lit = Rect::from_x_y_ranges(x_of(clip_start).round()..=x_of(clip_end).round(), body.y_range()).intersect(body);
    if lit.width() > 0.0 {
        painter.rect_filled(lit, 0.0, fade(color, 0.10));
    }
    // Past the end of the file there is nothing.
    let end_x = x_of(file_seconds);
    if end_x < body.right() {
        painter.rect_filled(body.with_min_x(end_x.max(body.left())), 0.0, theme.shade);
    }

    let step = ruler_step(span / f64::from(view.width()), 64.0);
    let mut mark = (from / step).ceil() * step;
    while mark <= to {
        let x = x_of(mark).round();
        if mark >= -1e-9 {
            paint::vline(&painter, x, ruler.bottom() - 5.0..=ruler.bottom(), theme.ink_faint);
            paint::vline(&painter, x, body.y_range(), theme.grid_fine);
            let label = Rect::from_min_max(pos2(x + 4.0, ruler.top()), pos2(x + 80.0, ruler.bottom()));
            let number = trill_noir::TextStyle { size: 10.0, ..theme.text.value };
            text::paint(&painter, label, Align2::LEFT_CENTER, number, &format_time(mark, step), theme.ink_faint);
        }
        mark += step;
    }

    // The waveform: the two channels one above the other where there is
    // room for both and there are two, otherwise their mix.
    let columns = trill_noir::waveform_columns(&painter, body.width());
    let found = wave.columns(from..to, columns);
    let gain = 10f32.powf(audio.gain_db / 20.0);
    let hair = paint::hair(&painter);
    let inside = |index: usize| {
        let second = from + (index as f64 + 0.5) / columns.max(1) as f64 * span;
        second >= clip_start && second < clip_end
    };
    let stereo = wave.facts.channels >= 2 && body.height() >= 110.0;
    let lanes: Vec<(Rect, Channel)> = if stereo {
        let (upper, lower) = body.split_top_bottom_at_fraction(0.5);
        paint::hline(&painter, body.x_range(), upper.bottom(), theme.hairline);
        vec![
            (upper.shrink2(vec2(0.0, 5.0)), |column| (column.left.min, column.left.max)),
            (lower.shrink2(vec2(0.0, 5.0)), |column| (column.right.min, column.right.max)),
        ]
    } else {
        vec![(body.shrink2(vec2(0.0, 8.0)), |column| (column.mixed.min, column.mixed.max))]
    };
    let mut mesh = Mesh::default();
    for (lane, channel) in &lanes {
        paint::hline(&painter, lane.x_range(), lane.center().y, theme.grid);
        // Lit and unlit columns are two bands, so that each keeps one
        // color from edge to edge.
        for (wanted, ink) in [(true, color), (false, fade(theme.ink_faint, 0.6))] {
            let mut start = 0;
            while start < found.len() {
                let Some(first) = (start..found.len()).find(|index| inside(*index) == wanted) else { break };
                let last = (first..found.len()).find(|index| inside(*index) != wanted).unwrap_or(found.len());
                let bands: Vec<(f32, f32)> = found[first..last].iter().map(channel).collect();
                let width = lane.width() / columns.max(1) as f32;
                let part = Rect::from_min_max(
                    pos2(lane.left() + first as f32 * width, lane.top()),
                    pos2(lane.left() + last as f32 * width, lane.bottom()),
                );
                trill_noir::waveform(&mut mesh, part, &bands, if wanted { gain } else { 1.0 }, ink, hair);
                start = last;
            }
        }
    }
    painter.add(Shape::mesh(Arc::new(mesh)));

    // The clip's edges and its fades, drawn as in the arrangement.
    for edge in [clip_start, clip_end] {
        let x = x_of(edge).round();
        if x >= body.left() && x <= body.right() {
            paint::vline(&painter, x, view.y_range(), theme.ink_dim);
        }
    }
    let per_beat = audio.seconds_per_beat(tempo);
    let fades = [(clip_start, clip_start + audio.fade_in * per_beat), (clip_end, clip_end - audio.fade_out * per_beat)];
    for (silent, full) in fades {
        let (silent, full) = (x_of(silent), x_of(full));
        if (full - silent).abs() < 1.0 {
            continue;
        }
        let point = |progress: f32| {
            pos2(silent + (full - silent) * progress, body.bottom() - body.height() * arrangement::fade_level(progress))
        };
        let curve: Vec<Pos2> = (0..=FADE_STEPS).map(|step| point(step as f32 / FADE_STEPS as f32)).collect();
        let mut shade = Mesh::default();
        for (index, point) in curve.iter().enumerate() {
            shade.colored_vertex(*point, theme.shade);
            shade.colored_vertex(pos2(point.x, body.top()), theme.shade);
            if index > 0 {
                let first = 2 * index as u32;
                shade.add_triangle(first - 2, first - 1, first);
                shade.add_triangle(first - 1, first + 1, first);
            }
        }
        painter.add(Shape::mesh(Arc::new(shade)));
        painter.add(Shape::line(curve, Stroke::new(1.0, theme.ink)));
    }

    // Where playback is, while it is inside the clip.
    let position = cx.live.position;
    if cx.live.playing && position >= clip.start && position < clip.end() {
        let second = clip_start + (position - clip.start) * per_beat;
        paint::vline(&painter, x_of(second).round(), view.y_range(), theme.signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with(audio: AudioContent, length: f64) -> (Project, ClipId) {
        let mut project = Project::new("t");
        project.set_tempo(120.0).unwrap();
        let track = project.add_track("A");
        let clip = project.add_audio_clip(track, "Loop", 4.0, length, audio).unwrap();
        (project, clip)
    }

    #[test]
    fn warping_keeps_the_stretch_of_the_recording_a_clip_plays() {
        // Two seconds of a loop recorded at 90, in a song at 120: four
        // beats unwarped, three once it follows the tempo.
        let audio = AudioContent { offset_seconds: 0.5, fade_in: 1.0, fade_out: 1.0, ..AudioContent::new("loop.wav") };
        let (mut project, clip) = project_with(audio, 4.0);
        rewarp(&mut project, clip, WarpMode::Beats, Some(90.0), 0.0).unwrap();
        let warped = project.clip(clip).unwrap().clone();
        assert!((warped.length - 3.0).abs() < 1e-9, "{}", warped.length);
        assert_eq!(warped.start, 4.0, "it stays where it is");
        let content = warped.audio().unwrap();
        assert_eq!((content.warp, content.source_tempo, content.offset_seconds), (WarpMode::Beats, Some(90.0), 0.5));
        assert_eq!(played(&warped, content, 120.0), (0.5, 2.5), "the same two seconds of the file");
        assert_eq!((content.fade_in, content.fade_out), (1.0, 1.0), "fades that still fit are left alone");

        // Another source tempo: the same seconds are now six beats.
        rewarp(&mut project, clip, WarpMode::Beats, Some(180.0), 0.0).unwrap();
        assert!((project.clip(clip).unwrap().length - 6.0).abs() < 1e-9);
        // And back off: four beats again.
        rewarp(&mut project, clip, WarpMode::Off, Some(180.0), 0.0).unwrap();
        assert!((project.clip(clip).unwrap().length - 4.0).abs() < 1e-9);
    }

    #[test]
    fn fades_shrink_with_a_clip_that_becomes_too_short_for_them() {
        let audio = AudioContent { fade_in: 2.0, fade_out: 2.0, ..AudioContent::new("loop.wav") };
        let (mut project, clip) = project_with(audio, 4.0);
        // Recorded at 60: the two seconds it covers are two beats.
        rewarp(&mut project, clip, WarpMode::Tonal, Some(60.0), 0.0).unwrap();
        let warped = project.clip(clip).unwrap();
        let content = warped.audio().unwrap();
        assert!((warped.length - 2.0).abs() < 1e-9);
        assert!((content.fade_in - 1.0).abs() < 1e-9 && (content.fade_out - 1.0).abs() < 1e-9);

        // What the project refuses changes nothing.
        let before = project.clone();
        assert!(rewarp(&mut project, clip, WarpMode::Beats, Some(5.0), 0.0).is_err());
        assert!(rewarp(&mut project, clip, WarpMode::Beats, Some(120.0), 99.0).is_err());
        assert_eq!(project.clip(clip).unwrap().audio(), before.clip(clip).unwrap().audio());
    }

    #[test]
    fn a_clip_opens_framed_on_what_it_plays() {
        let audio = AudioContent { offset_seconds: 2.0, ..AudioContent::new("take.wav") };
        let (project, clip) = project_with(audio, 4.0);
        let clip = project.clip(clip).unwrap();
        let content = clip.audio().unwrap();
        assert_eq!(played(clip, content, 120.0), (2.0, 4.0));
        let (from, to) = framed(clip, content, 120.0, 10.0);
        assert!(from < 2.0 && from > 1.8 && to > 4.0 && to < 4.2, "{from}..{to}");
        // At the edges of the file there is no margin to show.
        let (from, to) = framed(clip, content, 120.0, 4.0);
        assert!(from > 1.8 && to == 4.0, "{from}..{to}");
        let whole = AudioContent::new("take.wav");
        assert_eq!(framed(clip, &whole, 120.0, 2.0).0, 0.0);
    }

    #[test]
    fn the_ruler_counts_in_round_steps() {
        assert_eq!(ruler_step(0.01, 64.0), 1.0);
        assert_eq!(ruler_step(0.001, 64.0), 0.1);
        assert!((ruler_step(0.002, 64.0) - 0.2).abs() < 1e-12);
        assert!((ruler_step(0.0004, 64.0) - 0.05).abs() < 1e-12);
        assert_eq!(ruler_step(1.0, 64.0), 100.0);
        assert_eq!(format_time(1.25, 0.05), "1.25");
        assert_eq!(format_time(0.5, 0.1), "0.5");
        assert_eq!(format_time(12.0, 1.0), "12");
        assert_eq!(format_time(75.5, 0.5), "1:15.5");
        assert_eq!(format_time(120.0, 10.0), "2:00");
        assert_eq!(format_time(61.004, 0.002), "1:01.004");
    }
}
