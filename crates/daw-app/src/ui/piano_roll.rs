//! The Clip tab: properties of the selected clip and a piano roll for its
//! notes.
//!
//! Notes have no ids, so selection and drags identify them by value. A drag
//! keeps the notes as they were when it began and derives every step from
//! those originals, replacing the previous step's notes in the project.

use super::state::{AUDITION_LENGTH, Cx, FocusArea, NoteDrag};
use super::timeline::{self, GridLevel, PitchView, Snap, SpanHit, TimeView};
use super::widgets::{self, Icon};
use super::{theme, units};
use daw_core::pitch::pitch_name;
use daw_core::project::{ClipContent, MidiContent};
use daw_core::{Beats, Clip, ClipId, Note, TimeSignature, Track};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Mesh, PointerButton, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2,
    vec2,
};
use std::time::Instant;

pub(super) const SIDEBAR_WIDTH: f32 = 204.0;
pub(super) const KEYS_WIDTH: f32 = 64.0;
pub(super) const RULER_HEIGHT: f32 = 20.0;
/// Width of the zone at a note's end that resizes it, in pixels.
const RESIZE_HANDLE: f32 = 7.0;
const DEFAULT_VELOCITY: u8 = 100;
const SHORTEST_NOTE: Beats = 1.0 / 32.0;
const DRUM_KIT: &str = "builtin:drumkit";

const WHITE_KEY: Color32 = Color32::from_rgb(0xD9, 0xDD, 0xE4);
const BLACK_KEY: Color32 = Color32::from_rgb(0x24, 0x27, 0x2E);
const ROW_LIGHT: Color32 = Color32::from_rgb(0x17, 0x1A, 0x1F);
const ROW_DARK: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);

// ---- note editing, as pure functions ---------------------------------------

/// `notes` without one instance of each note in `remove`, plus `add`.
fn replace_notes(notes: &[Note], remove: &[Note], add: &[Note]) -> Vec<Note> {
    let mut result = notes.to_vec();
    for note in remove {
        if let Some(index) = result.iter().position(|candidate| candidate == note) {
            result.remove(index);
        }
    }
    result.extend_from_slice(add);
    result
}

/// `originals` moved by `beats` and `semitones`, held back as a group so no
/// note leaves the clip's start or the keyboard and their spacing survives.
fn moved_notes(originals: &[Note], beats: Beats, semitones: i32) -> Vec<Note> {
    let earliest = originals.iter().map(|note| note.start).fold(f64::INFINITY, f64::min);
    let (lowest, highest) = timeline::pitch_range(originals).unwrap_or((0, 127));
    let beats = beats.max(-earliest);
    let semitones = semitones.clamp(-(lowest as i32), 127 - highest as i32);
    originals
        .iter()
        .map(|note| Note {
            start: (note.start + beats).max(0.0),
            pitch: (note.pitch as i32 + semitones) as u8,
            ..*note
        })
        .collect()
}

/// `originals` lengthened by `beats`, none shorter than `shortest`.
fn resized_notes(originals: &[Note], beats: Beats, shortest: Beats) -> Vec<Note> {
    originals.iter().map(|note| Note { length: (note.length + beats).max(shortest), ..*note }).collect()
}

/// How far a move drag has carried the notes: the grabbed note follows the
/// pointer and lands on the grid, and every other note moves by the same
/// amount.
fn move_delta(grabbed: &Note, grab_beat: Beats, pointer_beat: Beats, step: Option<Beats>) -> Beats {
    timeline::snap_round(grabbed.start + pointer_beat - grab_beat, step) - grabbed.start
}

/// The notes that touch the rectangle spanned by two corners in beat and
/// pitch space.
fn notes_in_box(notes: &[Note], beats: (Beats, Beats), pitches: (u8, u8)) -> Vec<Note> {
    let (first, last) = (beats.0.min(beats.1), beats.0.max(beats.1));
    let (low, high) = (pitches.0.min(pitches.1), pitches.0.max(pitches.1));
    notes
        .iter()
        .filter(|note| (low..=high).contains(&note.pitch) && note.start < last && note.end() > first)
        .copied()
        .collect()
}

/// The length a loop gets when it is first switched on: the notes rounded
/// up to whole bars, but no longer than the clip.
fn default_loop_length(clip: &Clip, beats_per_bar: Beats) -> Beats {
    let content_end = clip.notes().iter().map(Note::end).fold(0.0, f64::max);
    let bars = (content_end / beats_per_bar - 1e-9).ceil().max(1.0);
    (bars * beats_per_bar).min(clip.length)
}

/// Name of a General MIDI drum the built-in kit plays, for labeling rows.
fn drum_name(pitch: u8) -> Option<&'static str> {
    Some(match pitch {
        35 | 36 => "Kick",
        37 => "Rim",
        38 | 40 => "Snare",
        39 => "Clap",
        41 | 43 => "Low Tom",
        42 | 44 => "Closed Hat",
        45 | 47 => "Mid Tom",
        46 => "Open Hat",
        48 | 50 => "High Tom",
        49 => "Crash",
        51 => "Ride",
        54 => "Tambourine",
        56 => "Cowbell",
        70 => "Shaker",
        75 => "Claves",
        _ => return None,
    })
}

// ---- layout ----------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
struct Geometry {
    /// Above the keys: vertical zoom buttons.
    corner: Rect,
    ruler: Rect,
    keys: Rect,
    grid: Rect,
    time: TimeView,
    pitch: PitchView,
    signature: TimeSignature,
    /// The snap grid, `None` when snapping is off.
    snap: Option<Beats>,
}

impl Geometry {
    fn new(area: Rect, time: TimeView, pitch: PitchView, signature: TimeSignature, snap: Snap) -> Self {
        let split_x = area.left() + KEYS_WIDTH;
        let split_y = area.top() + RULER_HEIGHT;
        Self {
            corner: Rect::from_min_max(area.min, pos2(split_x, split_y)),
            ruler: Rect::from_min_max(pos2(split_x, area.top()), pos2(area.right(), split_y)),
            keys: Rect::from_min_max(pos2(area.left(), split_y), pos2(split_x, area.bottom())),
            grid: Rect::from_min_max(pos2(split_x, split_y), area.max),
            time,
            pitch,
            signature,
            snap: snap.step(),
        }
    }

    fn x(&self, beat: Beats) -> f32 {
        self.time.beat_to_x(self.grid.left(), beat)
    }

    fn beat(&self, x: f32) -> Beats {
        self.time.x_to_beat(self.grid.left(), x)
    }

    fn y(&self, pitch: u8) -> f32 {
        self.pitch.pitch_to_y(self.grid.top(), pitch)
    }

    fn pitch_at(&self, y: f32) -> u8 {
        self.pitch.y_to_pitch(self.grid.top(), y)
    }

    fn note_rect(&self, note: &Note) -> Rect {
        let left = self.x(note.start);
        let top = self.y(note.pitch);
        Rect::from_min_max(pos2(left, top), pos2(self.x(note.end()).max(left + 2.0), top + self.pitch.row_height))
    }

    /// Lowest and highest pitch with a row on screen.
    fn visible_pitches(&self) -> (u8, u8) {
        (self.pitch_at(self.grid.bottom() - 0.5), self.pitch_at(self.grid.top()))
    }

    /// Spacing of the vertical lines: the snap grid while its lines are far
    /// enough apart to read, otherwise whatever fits.
    fn line_step(&self) -> Beats {
        match self.snap {
            Some(step) if step * self.time.pixels_per_beat >= 9.0 => step,
            _ => timeline::grid_step(self.time.pixels_per_beat, self.signature, 14.0),
        }
    }

    /// Width of a note's resize zone in beats.
    fn handle_beats(&self) -> Beats {
        RESIZE_HANDLE as f64 / self.time.pixels_per_beat
    }
}

// ---- the tab ---------------------------------------------------------------

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let area = ui.max_rect();
    let Some((track, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) else {
        placeholder(
            ui,
            area,
            "No clip selected",
            "Click a clip in the arrangement to edit it here, or double-click an empty lane to create one.",
        );
        return;
    };

    let sidebar_rect = Rect::from_min_max(area.min, pos2(area.left() + SIDEBAR_WIDTH, area.bottom()));
    let roll_rect = Rect::from_min_max(pos2(sidebar_rect.right(), area.top()), area.max);
    ui.painter().vline(sidebar_rect.right() - 0.5, area.y_range(), theme::hairline(theme::BORDER));
    sidebar(ui, cx, track, clip, sidebar_rect);

    match &clip.content {
        ClipContent::Midi(midi) => roll(ui, cx, track, clip, midi, roll_rect),
        ClipContent::Audio(audio) => {
            let name = audio.path.rsplit(['/', '\\']).next().unwrap_or(&audio.path);
            let detail = format!(
                "{}\nStarts {:.2} s into the file, gain {}. Audio clips have no notes to edit.",
                audio.path,
                audio.offset_seconds,
                units::format_db(audio.gain_db)
            );
            placeholder(ui, roll_rect, name, &detail);
        }
    }
}

fn placeholder(ui: &Ui, area: Rect, title: &str, detail: &str) {
    let painter = ui.painter_at(area);
    painter.text(area.center() - vec2(0.0, 12.0), Align2::CENTER_BOTTOM, title, theme::strong(15.0), theme::TEXT_DIM);
    let galley =
        painter.layout(detail.to_owned(), theme::font(12.0), theme::TEXT_FAINT, (area.width() - 48.0).min(460.0));
    painter.galley(pos2(area.center().x - galley.size().x / 2.0, area.center().y - 4.0), galley, theme::TEXT_FAINT);
}

/// Clip name, length, loop and the editing options.
fn sidebar(ui: &mut Ui, cx: &mut Cx, track: &Track, clip: &Clip, rect: Rect) {
    let signature = cx.snap.project.time_signature;
    let beats_per_bar = signature.beats_per_bar();
    let clip_id = clip.id;

    let layout = egui::Layout::top_down(egui::Align::Min);
    widgets::place(ui, rect.shrink2(vec2(14.0, 10.0)), "clip-sidebar", layout, |ui| {
        // Compact rows, so everything fits beside a short piano roll.
        ui.spacing_mut().item_spacing = vec2(8.0, 5.0);
        ui.spacing_mut().interact_size.y = 22.0;
        ui.spacing_mut().button_padding = vec2(8.0, 2.0);
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 16.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, theme::track_color(track.color));
            ui.add(
                egui::Label::new(egui::RichText::new(&track.name).font(theme::font(12.0)).color(theme::TEXT_DIM))
                    .truncate(),
            );
        });

        let width = ui.available_width() - 12.0;
        if let Some(name) =
            widgets::name_field(ui, clip_id, &clip.name, &mut cx.state.renaming_clip, theme::strong(13.0), width)
        {
            cx.edit(|project| {
                project.clip_mut(clip_id)?.name = name;
                Ok(())
            });
        }
        ui.add_space(2.0);

        let label = |ui: &mut Ui, text: &str| {
            ui.label(egui::RichText::new(text).font(theme::font(12.0)).color(theme::TEXT_DIM));
        };
        egui::Grid::new("clip-properties").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            label(ui, "Start");
            ui.label(timeline::format_position(clip.start, signature));
            ui.end_row();

            label(ui, "Length");
            let step = cx.state.piano.snap.step();
            let mut length = clip.length;
            let response = ui.add(length_field(&mut length)).on_hover_text(format!(
                "{}. Drag, or double-click to type.",
                timeline::format_length(clip.length, signature)
            ));
            if response.drag_started() {
                cx.begin_gesture();
            }
            if response.changed() {
                let (start, length) =
                    (clip.start, timeline::snap_round(length, step).max(step.unwrap_or(SHORTEST_NOTE)));
                cx.edit(|project| project.set_clip_span(clip_id, start, length));
            }
            ui.end_row();

            if let Some(midi) = clip.midi() {
                label(ui, "Loop");
                ui.horizontal(|ui| {
                    let mut looping = midi.loop_length.is_some();
                    if ui
                        .checkbox(&mut looping, "")
                        .on_hover_text("Repeat the first part of the clip to fill its length")
                        .changed()
                    {
                        let loop_length = looping.then(|| default_loop_length(clip, beats_per_bar));
                        cx.edit(|project| project.clip_mut(clip_id)?.set_loop_length(loop_length));
                    }
                    if let Some(mut loop_length) = midi.loop_length {
                        let response = ui.add(length_field(&mut loop_length));
                        if response.drag_started() {
                            cx.begin_gesture();
                        }
                        if response.changed() {
                            let loop_length =
                                timeline::snap_round(loop_length, step).max(step.unwrap_or(SHORTEST_NOTE));
                            cx.edit(|project| project.clip_mut(clip_id)?.set_loop_length(Some(loop_length)));
                        }
                    }
                });
                ui.end_row();

                label(ui, "Snap");
                egui::ComboBox::from_id_salt("snap").width(88.0).selected_text(cx.state.piano.snap.label()).show_ui(
                    ui,
                    |ui| {
                        for snap in Snap::ALL {
                            ui.selectable_value(&mut cx.state.piano.snap, snap, snap.label());
                        }
                    },
                );
                ui.end_row();

                label(ui, "Notes");
                let selected = cx.state.selected_notes.len();
                let count = midi.notes.len();
                ui.label(if selected > 0 { format!("{selected} of {count} selected") } else { count.to_string() });
                ui.end_row();

                if let Some(first) = cx.state.selected_notes.first() {
                    label(ui, "Velocity");
                    let mut velocity = first.velocity;
                    let response = ui.add(egui::DragValue::new(&mut velocity).range(1..=127).speed(0.5));
                    if response.drag_started() {
                        cx.begin_gesture();
                    }
                    if response.changed() {
                        set_velocity(cx, clip_id, velocity);
                    }
                    ui.end_row();
                }
            }
        });

        // The reminder is the first thing to go when the panel is short.
        if clip.midi().is_some() && ui.available_height() >= 46.0 {
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.label(
                    egui::RichText::new("Click to add a note, drag to move or stretch it, right-click to delete.")
                        .font(theme::font(11.0))
                        .color(theme::TEXT_FAINT),
                );
            });
        }
    });
}

/// A length in beats that can be dragged or typed.
fn length_field(length: &mut Beats) -> egui::DragValue<'_> {
    egui::DragValue::new(length)
        .range(SHORTEST_NOTE..=100_000.0)
        .speed(0.05)
        .max_decimals(3)
        .suffix(" beats")
        .update_while_editing(false)
}

fn set_velocity(cx: &mut Cx, clip: ClipId, velocity: u8) {
    let before = cx.state.selected_notes.clone();
    let after: Vec<Note> = before.iter().map(|note| Note { velocity, ..*note }).collect();
    if swap_notes(cx, clip, &before, &after) {
        cx.state.selected_notes = after;
    }
}

/// Replaces `remove` with `add` in the clip, as one edit.
fn swap_notes(cx: &mut Cx, clip: ClipId, remove: &[Note], add: &[Note]) -> bool {
    cx.edit(|project| {
        let clip = project.clip_mut(clip)?;
        let notes = replace_notes(clip.notes(), remove, add);
        clip.set_notes(notes)
    })
    .is_some()
}

pub fn delete_selected_notes(cx: &mut Cx) {
    let Some(clip) = cx.state.selected_clip else { return };
    let doomed = std::mem::take(&mut cx.state.selected_notes);
    if !doomed.is_empty() {
        swap_notes(cx, clip, &doomed, &[]);
    }
}

pub fn select_all_notes(cx: &mut Cx) {
    if let Some((_, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) {
        cx.state.selected_notes = clip.notes().to_vec();
    }
}

/// Moves the selected notes by whole steps: arrow keys.
pub fn nudge_selected_notes(cx: &mut Cx, steps: i32, semitones: i32) {
    let Some(clip) = cx.state.selected_clip else { return };
    let before = cx.state.selected_notes.clone();
    if before.is_empty() {
        return;
    }
    let step = cx.state.piano.snap.step().unwrap_or(0.25);
    let after = moved_notes(&before, steps as f64 * step, semitones);
    if after != before && swap_notes(cx, clip, &before, &after) {
        cx.state.selected_notes = after;
    }
}

fn roll(ui: &mut Ui, cx: &mut Cx, track: &Track, clip: &Clip, midi: &MidiContent, area: Rect) {
    let signature = cx.snap.project.time_signature;
    let view = &mut cx.state.piano;
    let inner = Geometry::new(area, view.time, view.pitch, signature, view.snap);

    // Frame a newly selected clip once: all of it across, its notes
    // centered vertically. After that the user's zoom is left alone.
    if view.framed != Some(clip.id) {
        view.framed = Some(clip.id);
        // A long clip that repeats a short phrase is framed on the phrase,
        // with a little of the repeat in view to show that it loops.
        let phrase = midi.loop_length.map_or(clip.length, |length| (length * 1.25).min(clip.length));
        view.time.fit(0.0, phrase, inner.grid.width());
        let (low, high) = timeline::pitch_range(&midi.notes).unwrap_or((57, 69));
        let rows = (high - low) as f32 + 9.0;
        view.pitch.row_height = (inner.grid.height() / rows).clamp(8.0, 16.0).round();
        view.pitch.center_on(low, high, inner.grid.height());
    }
    view.pitch.clamp(inner.grid.height());
    scroll_and_zoom(ui, cx, &inner);

    let view = &cx.state.piano;
    let geometry = Geometry { time: view.time, pitch: view.pitch, ..inner };
    let is_drums = track.devices.first().is_some_and(|device| device.uri == DRUM_KIT);

    let response = ui.interact(geometry.grid, ui.scope_id().with("notes"), Sense::click_and_drag());
    paint_grid(ui, cx, &geometry, track, clip, midi);
    paint_ruler(ui, &geometry, clip, midi);
    keys(ui, cx, &geometry, track, is_drums);
    corner(ui, cx, &geometry);
    grid_interaction(ui, cx, &geometry, track, clip, &midi.notes, &response);
}

/// Wheel scrolls pitches, Shift+wheel pans time, Ctrl+wheel zooms time and
/// Alt+wheel zooms pitch, all around the pointer.
fn scroll_and_zoom(ui: &Ui, cx: &mut Cx, geometry: &Geometry) {
    let area = geometry.corner.union(geometry.grid);
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else { return };
    if !area.contains(pointer) || !ui.rect_contains_pointer(area) {
        return;
    }
    let (scroll, zoom, alt) = ui.input(|input| (input.smooth_scroll_delta, input.zoom_delta(), input.modifiers.alt));
    let view = &mut cx.state.piano;
    let height = geometry.grid.height();
    if zoom != 1.0 {
        view.time.zoom_around(geometry.grid.left(), pointer.x, zoom as f64);
    }
    if alt {
        view.pitch.zoom_around(geometry.grid.top(), pointer.y, (scroll.y * 0.006).exp(), height);
    } else {
        view.pitch.scroll_pixels(scroll.y, height);
        view.time.pan_pixels(-scroll.x);
    }
}

// ---- painting --------------------------------------------------------------

fn paint_grid(ui: &Ui, cx: &Cx, geometry: &Geometry, track: &Track, clip: &Clip, midi: &MidiContent) {
    let grid = geometry.grid;
    let painter = ui.painter_at(grid);
    painter.rect_filled(grid, 0.0, ROW_LIGHT);

    let (low, high) = geometry.visible_pitches();
    for pitch in low..=high {
        let top = geometry.y(pitch);
        if timeline::is_black_key(pitch) {
            let row = Rect::from_x_y_ranges(grid.x_range(), top..=top + geometry.pitch.row_height);
            painter.rect_filled(row, 0.0, ROW_DARK);
        }
        // Octaves are separated below each C; E/F, the other pair of
        // neighboring white keys, gets a fainter line.
        let separator = match pitch % 12 {
            0 => Some(theme::GRID_BAR),
            5 => Some(theme::GRID_BEAT),
            _ => None,
        };
        if let Some(color) = separator {
            painter.hline(grid.x_range(), top + geometry.pitch.row_height - 0.5, theme::hairline(color));
        }
    }

    let (first_beat, last_beat) = geometry.time.visible(grid.width());
    timeline::for_each_grid_line(first_beat, last_beat, geometry.signature, geometry.line_step(), |beat, level| {
        let color = match level {
            GridLevel::Bar => theme::BORDER_STRONG,
            GridLevel::Beat => theme::GRID_BAR,
            GridLevel::Subdivision => theme::GRID_BEAT,
        };
        painter.vline(geometry.x(beat).round() + 0.5, grid.y_range(), theme::hairline(color));
    });

    // Only the first `sounding` beats of the notes are played: up to the
    // loop length if the clip loops, else up to the clip's end.
    let sounding = midi.loop_length.map_or(clip.length, |length| length.min(clip.length));
    let color = theme::track_color(track.color);
    let quiet = theme::mix(theme::BG_INSET, color, 0.42);
    let loud = theme::mix(color, Color32::WHITE, 0.12);
    let mut bodies = Mesh::default();
    let mut outlines = Vec::new();
    for note in &midi.notes {
        if note.start > last_beat {
            // Sorted by start: nothing further right is visible either.
            break;
        }
        if note.end() < first_beat || note.pitch < low || note.pitch > high {
            continue;
        }
        let rect = geometry.note_rect(note);
        let mut fill = theme::mix(quiet, loud, note.velocity as f32 / 127.0);
        if note.start >= sounding {
            fill = theme::fade(fill, 0.35);
        }
        // Half a pixel of inset leaves a seam between neighbors.
        bodies.add_colored_rect(rect.shrink(0.5).intersect(grid), fill);
        if cx.state.selected_notes.contains(note) {
            outlines.push(Shape::rect_stroke(rect, 2.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside));
        }
    }
    painter.add(Shape::mesh(bodies));
    painter.extend(outlines);

    // Past the end of the clip (or of its loop) nothing new plays.
    let end_x = geometry.x(sounding);
    if end_x < grid.right() {
        let beyond = Rect::from_min_max(pos2(end_x.max(grid.left()), grid.top()), grid.max);
        painter.rect_filled(beyond, 0.0, theme::fade(Color32::BLACK, 0.34));
        let edge = if midi.loop_length.is_some() { theme::ACCENT } else { theme::TEXT_FAINT };
        painter.vline(end_x.round() + 0.5, grid.y_range(), theme::hairline(edge));
    }

    if let Some(NoteDrag::Marquee { origin, .. }) = &cx.state.piano.drag
        && let Some(pointer) = ui.input(|input| input.pointer.latest_pos())
    {
        let band = Rect::from_two_pos(*origin, pointer);
        painter.rect_filled(band, 0.0, theme::fade(theme::ACCENT, 0.12));
        painter.rect_stroke(band, 0.0, theme::hairline(theme::ACCENT), StrokeKind::Inside);
    }

    // The song's playhead, mapped into the clip (and around its loop).
    let position = cx.live.position - clip.start;
    if position >= 0.0 && position < clip.length {
        let x = geometry.x(position % sounding.max(SHORTEST_NOTE)).round() + 0.5;
        painter.vline(x, grid.y_range(), theme::hairline(theme::PLAYHEAD));
    }
}

fn paint_ruler(ui: &Ui, geometry: &Geometry, clip: &Clip, midi: &MidiContent) {
    let ruler = geometry.ruler;
    let painter = ui.painter_at(ruler);
    painter.rect_filled(ruler, 0.0, theme::BG_PANEL);
    painter.hline(ruler.x_range(), ruler.bottom() - 0.5, theme::hairline(theme::BORDER));

    // Bars are counted from the start of the clip.
    let signature = geometry.signature;
    let (first_beat, last_beat) = geometry.time.visible(ruler.width());
    let label_step = timeline::grid_step(geometry.time.pixels_per_beat, signature, 52.0);
    timeline::for_each_grid_line(first_beat, last_beat, signature, label_step, |beat, level| {
        let x = geometry.x(beat);
        painter.vline(x.round() + 0.5, ruler.bottom() - 5.0..=ruler.bottom(), theme::hairline(theme::BORDER_STRONG));
        let color = if level == GridLevel::Bar { theme::TEXT_DIM } else { theme::TEXT_FAINT };
        painter.text(
            pos2(x + 4.0, ruler.center().y),
            Align2::LEFT_CENTER,
            timeline::format_bar_beat(beat, signature),
            theme::font(10.0),
            color,
        );
    });

    // A bar under the ruler marks the part of the clip that plays.
    let sounding = midi.loop_length.map_or(clip.length, |length| length.min(clip.length));
    let span =
        Rect::from_min_max(pos2(geometry.x(0.0), ruler.bottom() - 3.0), pos2(geometry.x(sounding), ruler.bottom()));
    let color = if midi.loop_length.is_some() { theme::ACCENT } else { theme::TEXT_FAINT };
    painter.rect_filled(span, 0.0, color);
}

/// The keyboard down the left side. Press a key to hear it.
fn keys(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, track: &Track, is_drums: bool) {
    let keys = geometry.keys;
    let response = ui.interact(keys, ui.scope_id().with("keys"), Sense::click_and_drag());
    let held = cx.state.audition.map(|audition| audition.pitch);
    let hovered = response.hover_pos().map(|pointer| geometry.pitch_at(pointer.y));

    let painter = ui.painter_at(keys);
    let row_height = geometry.pitch.row_height;
    painter.rect_filled(keys, 0.0, if is_drums { theme::BG_RAISED } else { WHITE_KEY });
    let (low, high) = geometry.visible_pitches();
    for pitch in low..=high {
        let row = Rect::from_min_size(pos2(keys.left(), geometry.y(pitch)), vec2(keys.width(), row_height));
        let lit = held == Some(pitch);
        if is_drums {
            paint_drum_row(&painter, row, pitch, lit, hovered == Some(pitch));
        } else {
            paint_key(&painter, row, pitch, lit, hovered == Some(pitch));
        }
    }
    painter.vline(keys.right() - 0.5, keys.y_range(), theme::hairline(theme::BORDER_STRONG));

    // Sliding along the keyboard with the button down plays each key.
    if response.is_pointer_button_down_on()
        && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
    {
        cx.state.focus = FocusArea::PianoRoll;
        let pitch = geometry.pitch_at(pointer.y);
        if held != Some(pitch) {
            cx.audition(track.id, pitch, DEFAULT_VELOCITY, None);
        }
    }
}

fn paint_key(painter: &egui::Painter, row: Rect, pitch: u8, lit: bool, hovered: bool) {
    let black = timeline::is_black_key(pitch);
    if black {
        // A black key is shorter; the white keys on either side meet
        // behind it, along the middle of its row.
        let key = row.with_max_x(row.left() + row.width() * 0.62);
        painter.hline(key.right()..=row.right(), row.center().y, theme::hairline(theme::fade(Color32::BLACK, 0.28)));
        let fill = if lit {
            theme::ACCENT
        } else if hovered {
            theme::BG_CONTROL_ACTIVE
        } else {
            BLACK_KEY
        };
        painter.rect_filled(key.shrink2(vec2(0.0, 0.5)), egui::CornerRadius { nw: 0, sw: 0, ne: 2, se: 2 }, fill);
    } else {
        if lit || hovered {
            let tint = if lit { theme::fade(theme::ACCENT, 0.55) } else { theme::fade(Color32::BLACK, 0.1) };
            painter.rect_filled(row, 0.0, tint);
        }
        // B/C and E/F are white keys side by side.
        if matches!(pitch % 12, 0 | 5) {
            painter.hline(row.x_range(), row.bottom() - 0.5, theme::hairline(theme::fade(Color32::BLACK, 0.28)));
        }
    }
    if pitch.is_multiple_of(12) && row.height() >= 8.0 {
        painter.text(
            row.right_center() - vec2(5.0, 0.0),
            Align2::RIGHT_CENTER,
            pitch_name(pitch),
            theme::font(row.height().clamp(8.0, 10.0)),
            theme::TEXT_ON_COLOR,
        );
    }
}

/// Drum tracks list their sounds by name instead of showing piano keys.
fn paint_drum_row(painter: &egui::Painter, row: Rect, pitch: u8, lit: bool, hovered: bool) {
    let name = drum_name(pitch);
    let fill = match (lit, hovered, name.is_some()) {
        (true, ..) => theme::fade(theme::ACCENT, 0.6),
        (false, true, _) => theme::BG_CONTROL_HOVER,
        (false, false, true) => theme::BG_CONTROL,
        (false, false, false) => theme::BG_RAISED,
    };
    painter.rect_filled(row.shrink2(vec2(0.0, 0.5)), 0.0, fill);
    if let Some(name) = name
        && row.height() >= 9.0
    {
        let text_area = row.shrink2(vec2(6.0, 0.0));
        let size = row.height().clamp(8.0, 10.5);
        widgets::text_in(painter, text_area, Align2::LEFT_CENTER, name, theme::font(size), theme::TEXT);
    }
}

/// Vertical zoom buttons above the keyboard.
fn corner(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let corner = geometry.corner;
    let painter = ui.painter_at(corner);
    painter.rect_filled(corner, 0.0, theme::BG_PANEL);
    painter.hline(corner.x_range(), corner.bottom() - 0.5, theme::hairline(theme::BORDER));
    painter.vline(corner.right() - 0.5, corner.y_range(), theme::hairline(theme::BORDER));

    let button = |index: f32| {
        Rect::from_min_size(pos2(corner.left() + 6.0 + index * 26.0, corner.top() + 1.0), vec2(24.0, 17.0))
    };
    let height = geometry.grid.height();
    let middle = geometry.grid.center().y;
    let mut zoom = |ui: &mut Ui, index: f32, icon: Icon, factor: f32, hint: &str| {
        if widgets::icon_button_at(ui, button(index), ui.scope_id().with(("row-zoom", icon)), icon, None)
            .on_hover_text(hint)
            .clicked()
        {
            cx.state.piano.pitch.zoom_around(geometry.grid.top(), middle, factor, height);
        }
    };
    zoom(ui, 0.0, Icon::Minus, 1.0 / 1.25, "Shorter rows (Alt+scroll)");
    zoom(ui, 1.0, Icon::Plus, 1.25, "Taller rows (Alt+scroll)");
}

// ---- interaction -----------------------------------------------------------

fn grid_interaction(
    ui: &Ui,
    cx: &mut Cx,
    geometry: &Geometry,
    track: &Track,
    clip: &Clip,
    notes: &[Note],
    response: &egui::Response,
) {
    let (shift, free) = ui.input(|input| (input.modifiers.shift, input.modifiers.alt));
    let step = if free { None } else { geometry.snap };
    let hit = |position: Pos2| {
        timeline::note_hit(notes, geometry.beat(position.x), geometry.pitch_at(position.y), geometry.handle_beats())
    };
    let timed_release = || Some(Instant::now() + AUDITION_LENGTH);

    if cx.state.piano.drag.is_none()
        && let Some(pointer) = response.hover_pos()
        && let Some((_, SpanHit::End)) = hit(pointer)
    {
        ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
    }

    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some((index, _)) = hit(pointer)
    {
        let doomed = notes[index];
        swap_notes(cx, clip.id, &[doomed], &[]);
        cx.state.selected_notes.retain(|note| *note != doomed);
    }

    if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        cx.state.focus = FocusArea::PianoRoll;
        match hit(pointer) {
            Some((index, _)) => {
                let note = notes[index];
                let selection = &mut cx.state.selected_notes;
                match (shift, selection.iter().position(|selected| *selected == note)) {
                    (true, Some(position)) => drop(selection.remove(position)),
                    (true, None) => selection.push(note),
                    (false, _) => *selection = vec![note],
                }
                cx.audition(track.id, note.pitch, note.velocity, timed_release());
            }
            None => {
                let note = Note {
                    pitch: geometry.pitch_at(pointer.y),
                    start: timeline::snap_floor(geometry.beat(pointer.x), step),
                    length: cx.state.piano.note_length,
                    velocity: DEFAULT_VELOCITY,
                };
                if swap_notes(cx, clip.id, &[], &[note]) {
                    cx.state.selected_notes = vec![note];
                    cx.audition(track.id, note.pitch, note.velocity, timed_release());
                }
            }
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
    {
        cx.state.focus = FocusArea::PianoRoll;
        cx.state.piano.drag = Some(match hit(press) {
            Some((index, part)) => {
                let grabbed = notes[index];
                if !cx.state.selected_notes.contains(&grabbed) {
                    cx.state.selected_notes = vec![grabbed];
                }
                let originals = cx.state.selected_notes.clone();
                cx.begin_gesture();
                match part {
                    SpanHit::Body => {
                        cx.audition(track.id, grabbed.pitch, grabbed.velocity, None);
                        NoteDrag::Move {
                            current: originals.clone(),
                            originals,
                            grabbed,
                            grab_beat: geometry.beat(press.x),
                        }
                    }
                    SpanHit::End => NoteDrag::Resize { current: originals.clone(), originals, grabbed },
                }
            }
            None => {
                let kept = if shift { cx.state.selected_notes.clone() } else { Vec::new() };
                NoteDrag::Marquee { origin: press, kept }
            }
        });
    }

    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(drag) = cx.state.piano.drag.clone()
    {
        let pointer_beat = geometry.beat(pointer.x);
        match drag {
            NoteDrag::Move { originals, current, grabbed, grab_beat } => {
                let beats = move_delta(&grabbed, grab_beat, pointer_beat, step);
                let semitones = geometry.pitch_at(pointer.y) as i32 - grabbed.pitch as i32;
                let target = moved_notes(&originals, beats, semitones);
                if target != current && swap_notes(cx, clip.id, &current, &target) {
                    // Hearing the new pitch is how you find the right one.
                    let now_at = target.iter().zip(&originals).find(|(_, original)| **original == grabbed);
                    if let Some((moved, _)) = now_at
                        && cx.state.audition.is_none_or(|audition| audition.pitch != moved.pitch)
                    {
                        cx.audition(track.id, moved.pitch, moved.velocity, None);
                    }
                    cx.state.selected_notes = target.clone();
                    cx.state.piano.drag = Some(NoteDrag::Move { originals, current: target, grabbed, grab_beat });
                }
            }
            NoteDrag::Resize { originals, current, grabbed } => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                let beats = timeline::snap_round(pointer_beat, step) - grabbed.end();
                let shortest = step.unwrap_or(SHORTEST_NOTE);
                let target = resized_notes(&originals, beats, shortest);
                if target != current && swap_notes(cx, clip.id, &current, &target) {
                    // New notes take the length the user last chose.
                    cx.state.piano.note_length = (grabbed.length + beats).max(shortest);
                    cx.state.selected_notes = target.clone();
                    cx.state.piano.drag = Some(NoteDrag::Resize { originals, current: target, grabbed });
                }
            }
            NoteDrag::Marquee { origin, kept } => {
                let beats = (geometry.beat(origin.x), pointer_beat);
                let pitches = (geometry.pitch_at(origin.y), geometry.pitch_at(pointer.y));
                let mut selection = kept;
                for note in notes_in_box(notes, beats, pitches) {
                    if !selection.contains(&note) {
                        selection.push(note);
                    }
                }
                cx.state.selected_notes = selection;
            }
        }
    }

    // Also ends a drag whose button was released outside the window.
    if cx.state.piano.drag.is_some() && !ui.input(|input| input.pointer.primary_down()) {
        cx.state.piano.drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::Project;

    fn note(pitch: u8, start: Beats, length: Beats) -> Note {
        Note { pitch, start, length, velocity: 100 }
    }

    #[test]
    fn replacing_removes_one_instance_per_request() {
        let twin = note(60, 0.0, 1.0);
        let notes = [twin, twin, note(64, 1.0, 1.0)];
        let result = replace_notes(&notes, &[twin], &[note(72, 2.0, 0.5)]);
        assert_eq!(result, [twin, note(64, 1.0, 1.0), note(72, 2.0, 0.5)]);
        // Asking to remove a note that is not there changes nothing else.
        assert_eq!(replace_notes(&notes, &[note(1, 9.0, 1.0)], &[]), notes);
    }

    #[test]
    fn moving_keeps_the_group_together_at_the_edges() {
        let chord = [note(60, 1.0, 1.0), note(64, 1.5, 1.0), note(67, 2.0, 1.0)];
        assert_eq!(moved_notes(&chord, 0.5, -12), [note(48, 1.5, 1.0), note(52, 2.0, 1.0), note(55, 2.5, 1.0)]);
        // Dragged far left and far up: stopped by the earliest and highest note.
        let stopped = moved_notes(&chord, -10.0, 100);
        assert_eq!(stopped, [note(120, 0.0, 1.0), note(124, 0.5, 1.0), note(127, 1.0, 1.0)]);
        assert_eq!(moved_notes(&chord, 0.0, -100)[0].pitch, 0);
        assert!(moved_notes(&[], 1.0, 1).is_empty());
    }

    #[test]
    fn resizing_respects_a_minimum_length() {
        let notes = [note(60, 0.0, 1.0), note(62, 1.0, 0.25)];
        assert_eq!(resized_notes(&notes, 0.5, 0.25), [note(60, 0.0, 1.5), note(62, 1.0, 0.75)]);
        assert_eq!(resized_notes(&notes, -0.5, 0.25), [note(60, 0.0, 0.5), note(62, 1.0, 0.25)]);
    }

    #[test]
    fn a_moved_note_lands_on_the_grid() {
        let grabbed = note(60, 1.0, 1.0);
        // Grabbed a little into the note, dragged 0.6 beats right.
        assert_eq!(move_delta(&grabbed, 1.3, 1.9, Some(0.25)), 0.5);
        assert_eq!(move_delta(&grabbed, 1.3, 1.35, Some(0.25)), 0.0, "small movements do not move the note");
        assert_eq!(move_delta(&grabbed, 1.3, -5.0, Some(0.25)), -1.0, "cannot start before the clip");
        assert!((move_delta(&grabbed, 1.3, 1.9, None) - 0.6).abs() < 1e-12);
        // A note that was off the grid is pulled onto it.
        let offgrid = note(60, 1.1, 1.0);
        assert!((move_delta(&offgrid, 1.2, 1.2, Some(0.25)) + 0.1).abs() < 1e-12);
    }

    #[test]
    fn the_marquee_selects_what_it_touches() {
        let notes = [note(60, 0.0, 1.0), note(64, 2.0, 1.0), note(67, 4.0, 1.0)];
        assert_eq!(notes_in_box(&notes, (0.5, 2.5), (59, 65)), [notes[0], notes[1]]);
        // The corners can be given in any order.
        assert_eq!(notes_in_box(&notes, (2.5, 0.5), (65, 59)), [notes[0], notes[1]]);
        assert_eq!(notes_in_box(&notes, (0.5, 2.5), (61, 63)), []);
        assert_eq!(notes_in_box(&notes, (1.0, 2.0), (0, 127)), [], "touching edges do not count");
    }

    #[test]
    fn loops_default_to_the_bars_that_have_notes() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let id = project.add_midi_clip(track, "c", 0.0, 16.0).unwrap();
        assert_eq!(default_loop_length(project.clip(id).unwrap(), 4.0), 4.0, "an empty clip loops one bar");
        project.clip_mut(id).unwrap().add_notes(&[note(60, 0.0, 1.0), note(60, 5.5, 2.5)]).unwrap();
        assert_eq!(default_loop_length(project.clip(id).unwrap(), 4.0), 8.0, "notes end exactly on bar 3");
        project.clip_mut(id).unwrap().add_notes(&[note(60, 30.0, 4.0)]).unwrap();
        assert_eq!(default_loop_length(project.clip(id).unwrap(), 4.0), 16.0, "never longer than the clip");
    }

    #[test]
    fn drum_rows_are_named() {
        assert_eq!(drum_name(36), Some("Kick"));
        assert_eq!(drum_name(42), Some("Closed Hat"));
        assert_eq!(drum_name(60), None);
    }

    fn geometry() -> Geometry {
        let area = Rect::from_min_size(pos2(200.0, 500.0), vec2(800.0, 260.0));
        let pitch = PitchView { top: 72.0, row_height: 12.0 };
        Geometry::new(
            area,
            TimeView { origin: 0.0, pixels_per_beat: 40.0 },
            pitch,
            TimeSignature::default(),
            Snap::Sixteenth,
        )
    }

    #[test]
    fn notes_map_to_rectangles_and_back() {
        let g = geometry();
        assert_eq!(g.grid.min, pos2(200.0 + KEYS_WIDTH, 500.0 + RULER_HEIGHT));
        let rect = g.note_rect(&note(71, 1.0, 0.5));
        assert_eq!(rect, Rect::from_min_size(g.grid.min + vec2(40.0, 0.0), vec2(20.0, 12.0)));
        assert_eq!(g.pitch_at(rect.center().y), 71);
        assert_eq!(g.beat(rect.left()), 1.0);
        // Very short notes stay wide enough to see and grab.
        assert_eq!(g.note_rect(&note(71, 1.0, 0.001)).width(), 2.0);
        assert_eq!(g.visible_pitches(), (52, 71));
    }

    #[test]
    fn grid_lines_follow_snap_until_they_crowd() {
        let mut g = geometry();
        assert_eq!(g.line_step(), 0.25, "sixteenths are 10 px apart");
        g.time.pixels_per_beat = 20.0;
        assert_eq!(g.line_step(), 1.0, "sixteenths at 5 px would be noise");
        g.snap = None;
        g.time.pixels_per_beat = 40.0;
        assert_eq!(g.line_step(), 0.5);
        assert_eq!(g.handle_beats(), RESIZE_HANDLE as f64 / 40.0);
    }
}
