//! The Clip tab: properties of the selected clip and a piano roll for its
//! notes, with a lane along the bottom that shows their velocities or one
//! of the clip's MIDI controllers (`controller_lane`).
//!
//! Notes have no ids, so selection and drags identify them by value. A drag
//! keeps the notes as they were when it began and derives every step from
//! those originals, replacing the previous step's notes in the project.

use super::state::{AUDITION_LENGTH, Clipboard, Cx, FocusArea, LaneShown, NoteDrag};
use super::timeline::{self, GridLevel, PitchView, Snap, SpanHit, TimeView};
use super::{colors, controller_lane, widgets};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Mesh, PointerButton, Pos2, Rangef, Rect, Sense, Shape, Ui, pos2, vec2,
};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use trill_core::pitch::pitch_name;
use trill_core::project::{ClipContent, MidiContent};
use trill_core::{Beats, Clip, ClipId, DeviceId, MeterMap, Note, TimeSignature, Track};
use trill_engine::SampleFile;
use trill_noir::{Bar, Chip, Icon, NumberField, Theme, fade, paint, text};

pub(super) const SIDEBAR_WIDTH: f32 = 204.0;
/// Width of the piano keys.
pub(super) const KEYS_WIDTH: f32 = 44.0;
/// Width of the column on a drum track, which lists the kit's sounds by
/// name instead.
const DRUM_KEYS_WIDTH: f32 = 88.0;
pub(super) const RULER_HEIGHT: f32 = 18.0;
/// Height of the lane under the notes, below its strip of choices: tall
/// enough to draw a curve in.
pub(super) const VELOCITY_LANE: f32 = 56.0;
/// Height of the strip that says what the lane shows.
pub(super) const LANE_HEADER: f32 = controller_lane::HEADER;
/// Space kept clear above the tallest stem and below every stem's foot.
const STEM_HEADROOM: f32 = 8.0;
const STEM_FOOT: f32 = 4.0;
/// How close, in pixels, the pointer must be to a stem to grab it.
const STEM_GRAB: f32 = 6.0;
/// Width of the zone at a note's end that resizes it, in pixels.
const RESIZE_HANDLE: f32 = 7.0;
const SHORTEST_NOTE: Beats = 1.0 / 32.0;
pub(super) const DEFAULT_VELOCITY: u8 = 100;
const DRUM_KIT: &str = "builtin:drumkit";
const SIDEBAR_PADDING: f32 = 10.0;

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

/// `originals` with `change` added to every velocity, each kept in 1..=127.
fn revoiced_notes(originals: &[Note], change: i32) -> Vec<Note> {
    originals
        .iter()
        .map(|note| Note { velocity: (note.velocity as i32 + change).clamp(1, 127) as u8, ..*note })
        .collect()
}

/// `notes` with their starts moved to the nearest line of the grid.
fn quantized_notes(notes: &[Note], step: Beats) -> Vec<Note> {
    notes.iter().map(|note| Note { start: timeline::snap_round(note.start, Some(step)), ..*note }).collect()
}

/// `notes` each held until the next one starts, so the line has no gaps.
/// Notes that start together (a chord) are all held until the note after
/// them; the last notes keep their length.
fn legato_notes(notes: &[Note]) -> Vec<Note> {
    let mut starts: Vec<Beats> = notes.iter().map(|note| note.start).collect();
    starts.sort_by(f64::total_cmp);
    starts.dedup();
    notes
        .iter()
        .map(|note| match starts.iter().find(|start| **start > note.start + 1e-9) {
            Some(next) => Note { length: next - note.start, ..*note },
            None => *note,
        })
        .collect()
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

/// Where the top of a view `rows` tall has to be for the pitches
/// `low..=high` to be in it, moving it from `top` by as little as that
/// takes. A range taller than the view is shown from its lowest pitch up.
fn revealed_top(top: f32, rows: f32, low: u8, high: u8) -> f32 {
    let (low, above) = (low as f32, high as f32 + 1.0);
    if top - rows > low || above - low > rows { low + rows } else { top.max(above) }
}

/// A name from a drum map as a label: `closed_hat_edge` is "Closed Hat Edge".
fn words(name: &str) -> String {
    let capital = |word: &str| {
        let mut letters = word.chars();
        letters.next().map(|first| first.to_uppercase().chain(letters).collect::<String>()).unwrap_or_default()
    };
    name.split('_').filter(|word| !word.is_empty()).map(capital).collect::<Vec<_>>().join(" ")
}

/// What the row of `pitch` is called on a drum track: by the track's own
/// map; on a drum rack by the file on that pad, where `samples` has it;
/// else as General MIDI calls it, of which the built-in kit plays a part.
/// `None` for a key that plays no drum anybody knows of.
fn drum_row_name(track: &Track, samples: &HashMap<DeviceId, Vec<SampleFile>>, pitch: u8) -> Option<String> {
    let profile = &track.profile;
    if let Some(name) = profile.sound_name(pitch) {
        return Some(words(name));
    }
    let instrument = track.devices.first();
    // A pad of a drum rack is the sound that was put on it.
    let slot = format!("pad {pitch}");
    let pad = instrument
        .filter(|device| device.uri == super::sample_panels::DRUM_RACK)
        .and_then(|rack| samples.get(&rack.id)?.iter().find(|file| file.slot == slot));
    if let Some(file) = pad {
        return Path::new(&file.path).file_stem().map(|stem| stem.to_string_lossy().into_owned());
    }
    // A drum the track's map keeps on another key is not on this one.
    let general = trill_core::pitch::drum_name(pitch);
    if general.is_some_and(|name| profile.sounds.contains_key(name)) {
        return None;
    }
    match instrument {
        // The built-in kit says what it plays itself, also on the keys it doubles a drum on.
        Some(device) if device.uri == DRUM_KIT => kit_drum_name(pitch).map(str::to_owned),
        _ => general.map(words),
    }
}

/// What a key of a pitched track is, where the track knows that it is not
/// a note like the others: a keyswitch, a noise, a key outside the range.
fn key_meaning(track: &Track, pitch: u8) -> Option<String> {
    let profile = &track.profile;
    let key = pitch_name(pitch);
    if let Some(name) = profile.keyswitch_name(pitch) {
        return Some(format!("{key} is a keyswitch ({name}): it changes how the notes are played and sounds nothing"));
    }
    if let Some(name) = profile.sound_name(pitch) {
        return Some(format!("{key} plays a noise ({name}), not a note"));
    }
    let (low, high) = profile.range.filter(|_| !profile.in_range(pitch))?;
    Some(format!("{key} is outside what this instrument plays ({}-{})", pitch_name(low), pitch_name(high)))
}

/// Name of a General MIDI drum the built-in kit plays, for labeling rows.
fn kit_drum_name(pitch: u8) -> Option<&'static str> {
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
    /// The strip above the lane that says what it shows, across the full
    /// width under keys and grid.
    lane_header: Rect,
    /// The lane (velocities, or a controller's curve), under its strip.
    velocity: Rect,
    time: TimeView,
    pitch: PitchView,
    /// The time signature the clip begins in, and its bars as the song
    /// has them under it, counted from where its content begins.
    signature: TimeSignature,
    meter: timeline::FewBars,
    /// The snap grid, `None` when snapping is off.
    snap: Option<Beats>,
}

impl Geometry {
    fn new(
        area: Rect,
        keys_width: f32,
        time: TimeView,
        pitch: PitchView,
        signature: TimeSignature,
        snap: Snap,
    ) -> Self {
        let split_x = area.left() + keys_width;
        let split_y = area.top() + RULER_HEIGHT;
        // A panel dragged very short gives the lane up before the notes.
        let roomy = area.height() >= RULER_HEIGHT + LANE_HEADER + VELOCITY_LANE + 60.0;
        let (strip, lane) = if roomy { (LANE_HEADER, VELOCITY_LANE) } else { (0.0, 0.0) };
        let lane_top = area.bottom() - lane;
        let strip_top = lane_top - strip;
        Self {
            corner: Rect::from_min_max(area.min, pos2(split_x, split_y)),
            ruler: Rect::from_min_max(pos2(split_x, area.top()), pos2(area.right(), split_y)),
            keys: Rect::from_min_max(pos2(area.left(), split_y), pos2(split_x, strip_top)),
            grid: Rect::from_min_max(pos2(split_x, split_y), pos2(area.right(), strip_top)),
            lane_header: Rect::from_min_max(pos2(area.left(), strip_top), pos2(area.right(), lane_top)),
            velocity: Rect::from_min_max(pos2(area.left(), lane_top), area.max),
            time,
            pitch,
            signature,
            meter: timeline::FewBars::constant(signature),
            snap: snap.step(),
        }
    }

    /// The same, for a clip whose bars are `meter`.
    fn in_bars(self, meter: &MeterMap) -> Self {
        Self { signature: meter.signature_at(0.0), meter: timeline::FewBars::of(meter), ..self }
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

    /// How far a stem of full velocity rises above its foot.
    fn stem_travel(&self) -> f32 {
        (self.velocity.height() - STEM_HEADROOM - STEM_FOOT).max(1.0)
    }

    /// Screen y of the top of a stem for `velocity`.
    fn stem_top(&self, velocity: u8) -> f32 {
        self.velocity.bottom() - STEM_FOOT - velocity as f32 / 127.0 * self.stem_travel()
    }

    /// The velocity whose stem would end at screen position `y`.
    fn velocity_at(&self, y: f32) -> u8 {
        let position = (self.velocity.bottom() - STEM_FOOT - y) / self.stem_travel();
        (position * 127.0).round().clamp(1.0, 127.0) as u8
    }

    /// The note whose stem is nearest to screen position `x`, if one is
    /// within reach. Among notes that start together the loudest wins: its
    /// stem is the one whose head can be seen.
    fn stem_at(&self, notes: &[Note], x: f32) -> Option<Note> {
        let reach = |note: &&Note| (self.x(note.start) - x).abs() <= STEM_GRAB;
        notes.iter().filter(reach).copied().min_by(|a, b| {
            let distance = |note: &Note| (self.x(note.start) - x).abs();
            distance(a).total_cmp(&distance(b)).then(b.velocity.cmp(&a.velocity))
        })
    }
}

// ---- the tab ---------------------------------------------------------------

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    let Some((track, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) else {
        placeholder(
            ui,
            theme,
            area,
            theme.text.caps,
            "No clip selected",
            "Click a clip in the arrangement to edit it here, or double-click an empty lane to create one.",
        );
        return;
    };

    let sidebar_rect = Rect::from_min_max(area.min, pos2(area.left() + SIDEBAR_WIDTH, area.bottom()));
    let roll_rect = Rect::from_min_max(pos2(sidebar_rect.right(), area.top()), area.max);
    sidebar(ui, cx, clip, sidebar_rect);

    match &clip.content {
        ClipContent::Midi(midi) => roll(ui, cx, track, clip, midi, roll_rect),
        ClipContent::Audio(audio) => super::audio_clip::show(ui, cx, track, clip, audio, roll_rect),
    }
}

/// A title and a line of explanation in the middle of `area`.
fn placeholder(ui: &Ui, theme: &Theme, area: Rect, title_style: trill_noir::TextStyle, title: &str, detail: &str) {
    let painter = ui.painter_at(area);
    let middle = area.center().y.round();
    let upper = Rect::from_min_max(pos2(area.left() + 16.0, middle - 20.0), pos2(area.right() - 16.0, middle - 2.0));
    text::paint(&painter, upper, Align2::CENTER_CENTER, title_style, title, theme.ink_dim);
    let lower = Rect::from_min_max(pos2(area.left() + 16.0, middle + 2.0), pos2(area.right() - 16.0, middle + 22.0));
    let quiet = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
    text::paint(&painter, lower, Align2::CENTER_CENTER, quiet, detail, theme.ink_faint);
}

/// Stacks the sidebar's controls from the top, each as wide as the column.
pub(super) struct Stack {
    left: f32,
    width: f32,
    top: f32,
    gap: f32,
}

impl Stack {
    /// The next row, `height` tall.
    pub(super) fn row(&mut self, height: f32) -> Rect {
        let rect = Rect::from_min_size(pos2(self.left, self.top), vec2(self.width, height));
        self.top += height + self.gap;
        rect
    }

    /// A section heading: micro caps over a rule.
    pub(super) fn heading(&mut self, ui: &Ui, theme: &Theme, title: &str, first: bool) {
        if !first {
            self.top += 4.0;
        }
        let rect = self.row(16.0);
        text::paint(
            ui.painter(),
            rect.with_max_y(rect.bottom() - 2.0),
            Align2::LEFT_CENTER,
            theme.text.micro,
            title,
            theme.ink_faint,
        );
        paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
        self.top += self.gap;
    }
}

/// Clip name, start, length and loop; the grid; the selected notes.
fn sidebar(ui: &mut Ui, cx: &mut Cx, clip: &Clip, rect: Rect) {
    let theme = cx.theme;
    // Where the clip is in the song, and the time signature it starts in.
    let bars = cx.snap.project.meter_map();
    let signature = bars.signature_at(clip.start);
    let beats_per_bar = signature.beats_per_bar();
    let clip_id = clip.id;
    let (bar, chip, gap) = (theme.metric.bar, theme.metric.chip, theme.metric.gap);
    let id = ui.scope_id().with("clip-sidebar");

    ui.painter().rect_filled(rect, 0.0, theme.panel);
    paint::vline_left_of(ui.painter(), rect.right(), rect.y_range(), theme.hairline);
    let clip_rect = rect.intersect(ui.clip_rect());
    let mut ui = ui.new_child(egui::UiBuilder::new().id_salt("clip-sidebar").max_rect(rect));
    ui.set_clip_rect(clip_rect);
    let ui = &mut ui;
    let mut stack = Stack {
        left: rect.left() + SIDEBAR_PADDING,
        width: rect.width() - 2.0 * SIDEBAR_PADDING - 1.0,
        top: rect.top() + 8.0,
        gap,
    };

    stack.heading(ui, theme, "Clip", true);
    let name_row = stack.row(22.0);
    if let Some(name) = widgets::name_field(ui, theme, name_row, clip_id, &clip.name, &mut cx.state.renaming_clip) {
        cx.edit(|project| {
            project.clip_mut(clip_id)?.name = name;
            Ok(())
        });
    }

    // Where the clip starts is read here and changed by dragging the clip.
    let start_row = stack.row(bar);
    paint::frame(ui.painter(), start_row, theme.hairline);
    let inner = start_row.shrink2(vec2(theme.metric.pad + 2.0, 0.0));
    text::paint(ui.painter(), inner, Align2::LEFT_CENTER, theme.text.label, "Start", theme.ink_faint);
    let start = timeline::format_position(clip.start, &bars).replace('.', ". ");
    text::paint(ui.painter(), inner, Align2::RIGHT_CENTER, theme.text.value, &start, theme.ink);
    let response = ui.interact(start_row, id.with("start"), Sense::hover());
    cx.hint(&response, "Where the clip starts: bar, beat, sixteenth \u{00B7} drag the clip to move it");

    let step = cx.state.piano.snap.step();
    let shown = format!("{:.2}", clip.length);
    let field = length_field(theme, "Length", "Clip length", &shown, clip.length);
    let output = trill_noir::number_field(ui, theme, stack.row(bar), id.with("length"), &field);
    cx.hint(
        &output.response,
        format!("{} \u{00B7} drag, or double-click to type", timeline::format_length(clip.length, signature)),
    );
    if output.response.drag_started() {
        cx.begin_gesture();
    }
    if let Some(length) = output.value {
        let wanted = timeline::snap_round(length, step).max(step.unwrap_or(SHORTEST_NOTE));
        // An audio clip does not grow past the end of its file.
        let (start, length) = (clip.start, super::arrangement::allowed_length(cx, clip, wanted));
        if length != clip.length {
            cx.edit(|project| project.set_clip_span(clip_id, start, length));
        }
    }

    let toggles = stack.row(chip);
    let mut x = toggles.left();
    let mut toggle = |ui: &Ui, label: &str, on: bool| {
        let spec = Chip::new(label, on);
        let chip_rect = Rect::from_min_size(pos2(x, toggles.top()), vec2(spec.width(ui, theme), chip));
        x = chip_rect.right() + gap - 1.0;
        trill_noir::chip(ui, theme, chip_rect, id.with(("toggle", label)), &spec)
    };
    let looping = clip.midi().and_then(|midi| midi.loop_length);
    if clip.midi().is_some() {
        let response = toggle(ui, "Loop", looping.is_some());
        cx.hint(&response, "Repeat the first part of the clip to fill its length");
        if response.clicked() {
            let loop_length = looping.is_none().then(|| default_loop_length(clip, beats_per_bar));
            cx.edit(|project| project.clip_mut(clip_id)?.set_loop_length(loop_length));
        }
    }
    let response = toggle(ui, "Mute", clip.muted);
    cx.hint(&response, "Silence this clip without removing it");
    if response.clicked() {
        let muted = !clip.muted;
        cx.edit(|project| {
            project.clip_mut(clip_id)?.muted = muted;
            Ok(())
        });
    }
    if let Some(loop_length) = looping {
        let shown = format!("{loop_length:.2}");
        let field = length_field(theme, "Loop", "Loop length", &shown, loop_length);
        let output = trill_noir::number_field(ui, theme, stack.row(bar), id.with("loop-length"), &field);
        cx.hint(&output.response, "How much of the clip repeats \u{00B7} drag, or double-click to type");
        if output.response.drag_started() {
            cx.begin_gesture();
        }
        if let Some(length) = output.value {
            let length = timeline::snap_round(length, step).max(step.unwrap_or(SHORTEST_NOTE));
            if length != loop_length {
                cx.edit(|project| project.clip_mut(clip_id)?.set_loop_length(Some(length)));
            }
        }
    }

    let midi = match &clip.content {
        ClipContent::Midi(midi) => midi,
        ClipContent::Audio(audio) => return super::audio_clip::sidebar(ui, cx, clip, audio, &mut stack),
    };

    stack.heading(ui, theme, "Grid", false);
    // Straight note values on one line, triplets and "off" on the next.
    for (line, snaps) in [&Snap::ALL[..4], &Snap::ALL[4..]].into_iter().enumerate() {
        let labels: Vec<&str> = snaps.iter().map(|snap| snap.label()).collect();
        let current = snaps.iter().position(|snap| *snap == cx.state.piano.snap);
        let row = stack.row(chip);
        let (clicked, _) =
            trill_noir::chip_row(ui, theme, row.min, row.width(), id.with(("grid", line)), &labels, current, false);
        if ui.rect_contains_pointer(row) {
            cx.state.hint.set("The grid notes snap to \u{00B7} hold Alt while dragging to ignore it");
        }
        if let Some(index) = clicked {
            cx.state.piano.snap = snaps[index];
        }
    }

    let selected = cx.state.selected_notes.len();
    let title = if selected > 0 { format!("Notes \u{00B7} {selected} selected") } else { "Notes".to_owned() };
    stack.heading(ui, theme, &title, false);
    // With notes selected the bar sets their velocity; without, the
    // velocity the next note will be given.
    let velocity = cx.state.selected_notes.first().map_or(cx.state.piano.note_velocity, |note| note.velocity);
    let shown = velocity.to_string();
    let default = (DEFAULT_VELOCITY as f32 - 1.0) / 126.0;
    let bar_spec = Bar::new("Velocity", &shown, (velocity as f32 - 1.0) / 126.0).default(default);
    let output = trill_noir::bar(ui, theme, stack.row(bar), id.with("velocity"), &bar_spec);
    cx.hint(
        &output.response,
        if selected > 0 {
            "Velocity of the selected notes"
        } else {
            "Velocity new notes are given \u{00B7} select notes to change theirs"
        },
    );
    if output.pressed {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac {
        let velocity = if output.reset { DEFAULT_VELOCITY } else { (1.0 + position * 126.0).round() as u8 };
        cx.state.piano.note_velocity = velocity;
        set_velocity(cx, clip_id, velocity);
    }

    let actions = stack.row(chip);
    let mut x = actions.left();
    let mut action = |ui: &Ui, label: &str| {
        let spec = Chip::action(label);
        let chip_rect = Rect::from_min_size(pos2(x, actions.top()), vec2(spec.width(ui, theme), chip));
        x = chip_rect.right() + gap - 1.0;
        trill_noir::chip(ui, theme, chip_rect, id.with(("action", label)), &spec)
    };
    let scope = if selected > 0 { "the selected notes" } else { "every note" };
    let response = action(ui, "Quantize");
    cx.hint(&response, format!("Move {scope} onto the grid \u{00B7} Ctrl+U"));
    if response.clicked() {
        quantize(cx);
    }
    let response = action(ui, "Legato");
    cx.hint(&response, format!("Hold {scope} until the next one starts"));
    if response.clicked() {
        reshape_notes(cx, clip_id, &midi.notes, legato_notes);
    }

    // A summary at the foot of the column, when there is room for it.
    let foot =
        Rect::from_min_max(pos2(stack.left, rect.bottom() - 22.0), pos2(stack.left + stack.width, rect.bottom() - 6.0));
    if foot.top() >= stack.top + 4.0 {
        let summary = match timeline::pitch_range(&midi.notes) {
            Some((low, high)) => {
                format!("{} notes \u{00B7} {} \u{2013} {}", midi.notes.len(), pitch_name(low), pitch_name(high))
            }
            None if !midi.controllers.is_empty() => "No notes \u{00B7} controllers only".to_owned(),
            None => "No notes yet \u{00B7} click the grid to add one".to_owned(),
        };
        text::paint(ui.painter(), foot, Align2::LEFT_CENTER, theme.text.micro, &summary, theme.ink_faint);
    }
}

/// A length in beats that can be dragged or typed.
fn length_field<'a>(theme: &Theme, label: &'a str, name: &'a str, shown: &'a str, value: Beats) -> NumberField<'a> {
    NumberField {
        label,
        text: shown,
        unit: "beats",
        value,
        range: SHORTEST_NOTE..=100_000.0,
        per_point: 0.05,
        style: theme.text.value,
        name,
    }
}

fn set_velocity(cx: &mut Cx, clip: ClipId, velocity: u8) {
    let before = cx.state.selected_notes.clone();
    let after: Vec<Note> = before.iter().map(|note| Note { velocity, ..*note }).collect();
    if after != before && swap_notes(cx, clip, &before, &after) {
        cx.state.selected_notes = after;
    }
}

/// Applies `reshape` to the selected notes, or to all of them when none is
/// selected, as one edit.
fn reshape_notes(cx: &mut Cx, clip: ClipId, all: &[Note], reshape: impl Fn(&[Note]) -> Vec<Note>) {
    let selection = !cx.state.selected_notes.is_empty();
    let before = if selection { cx.state.selected_notes.clone() } else { all.to_vec() };
    let after = reshape(&before);
    if after != before && swap_notes(cx, clip, &before, &after) && selection {
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

/// `notes` with their starts counted from the earliest of them: how notes
/// go onto the clipboard, ready to be put down anywhere.
fn rebased_notes(notes: &[Note]) -> Vec<Note> {
    let earliest = notes.iter().map(|note| note.start).fold(f64::INFINITY, f64::min);
    notes.iter().map(|note| Note { start: note.start - earliest, ..*note }).collect()
}

/// How long a group of notes is, from the first start to the last end,
/// rounded up to whole grid steps so that repeats of it stay in time.
fn phrase_length(notes: &[Note], step: Option<Beats>) -> Beats {
    let start = notes.iter().map(|note| note.start).fold(f64::INFINITY, f64::min);
    let end = notes.iter().map(Note::end).fold(f64::NEG_INFINITY, f64::max);
    let length = (end - start).max(0.0);
    match step {
        Some(step) if step > 0.0 => ((length / step) - 1e-9).ceil().max(1.0) * step,
        _ => length,
    }
}

/// Puts the selected notes on the clipboard.
pub fn copy_notes(cx: &mut Cx) {
    if cx.state.selected_notes.is_empty() {
        return;
    }
    let notes = rebased_notes(&cx.state.selected_notes);
    let count = notes.len();
    cx.state.copied_text =
        Some(if count == 1 { "1 note from Trill".to_owned() } else { format!("{count} notes from Trill") });
    cx.state.clipboard = Some(Clipboard::Notes(notes));
}

pub fn cut_notes(cx: &mut Cx) {
    copy_notes(cx);
    delete_selected_notes(cx);
}

/// Pastes the copied notes at the roll's cursor, selects them, and moves
/// the cursor past them so pasting again follows on.
pub fn paste_notes(cx: &mut Cx) {
    let Some(clip) = cx.state.selected_clip else { return };
    let Some(Clipboard::Notes(copied)) = cx.state.clipboard.clone() else { return };
    let step = cx.state.piano.snap.step();
    let at = timeline::snap_round(cx.state.piano.cursor, step);
    let pasted: Vec<Note> = copied.iter().map(|note| Note { start: note.start + at, ..*note }).collect();
    if swap_notes(cx, clip, &[], &pasted) {
        cx.state.piano.cursor = at + phrase_length(&pasted, step);
        cx.state.selected_notes = pasted;
    }
}

/// Copies the selected notes to right after themselves and selects the
/// copies, so repeating the command writes a phrase out again and again.
pub fn duplicate_notes(cx: &mut Cx) {
    let Some(clip) = cx.state.selected_clip else { return };
    let chosen = cx.state.selected_notes.clone();
    if chosen.is_empty() {
        return;
    }
    let length = phrase_length(&chosen, cx.state.piano.snap.step());
    let copies: Vec<Note> = chosen.iter().map(|note| Note { start: note.start + length, ..*note }).collect();
    if swap_notes(cx, clip, &[], &copies) {
        cx.state.selected_notes = copies;
    }
}

/// Moves the selected notes (or all of them, with none selected) onto the
/// grid: `Ctrl+U`, and the sidebar's Quantize.
pub fn quantize(cx: &mut Cx) {
    let Some(step) = cx.state.piano.snap.step() else {
        cx.state.toasts.info("Choose a grid to quantize to: it is off");
        return;
    };
    let Some((_, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) else { return };
    let (id, all) = (clip.id, clip.notes().to_vec());
    reshape_notes(cx, id, &all, |notes| quantized_notes(notes, step));
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
    // The clip's bars are the song's from where its content begins.
    let meter = cx.snap.project.meter_map().counted_from((clip.start - midi.start_offset).max(0.0));
    let signature = meter.signature_at(0.0);
    let is_drums = cx.snap.plays_drums(track.id);
    let keys_width = if is_drums { DRUM_KEYS_WIDTH } else { KEYS_WIDTH };
    let view = &mut cx.state.piano;
    let inner = Geometry::new(area, keys_width, view.time, view.pitch, signature, view.snap).in_bars(&meter);

    // Frame a newly selected clip once: all of it across, its notes
    // centered vertically. After that the user's zoom is left alone.
    if view.framed != Some(clip.id) {
        view.framed = Some(clip.id);
        // A long clip that repeats a short phrase is framed on the phrase,
        // with a little of the repeat in view to show that it loops.
        let (from, to) = sounding_span(clip, midi);
        let phrase = midi.loop_length.map_or(to - from, |length| (length * 1.25).min(clip.length));
        view.time.fit(from, from + phrase, inner.grid.width());
        let (low, high) = timeline::pitch_range(&midi.notes).unwrap_or((57, 69));
        let rows = (high - low) as f32 + 9.0;
        view.pitch.row_height = (inner.grid.height() / rows).clamp(8.0, 12.0).round();
        view.pitch.center_on(low, high, inner.grid.height());
    }
    view.pitch.clamp(inner.grid.height());
    // The computer keyboard moved to another octave: the keys it plays
    // now are brought into view, by as little as it takes.
    if cx.keyboard.take_reveal() {
        let rows = inner.grid.height() / view.pitch.row_height;
        view.pitch.top = revealed_top(view.pitch.top, rows, cx.keyboard.lowest(), cx.keyboard.highest());
        view.pitch.clamp(inner.grid.height());
    }
    scroll_and_zoom(ui, cx, &inner);

    let view = &cx.state.piano;
    let geometry = Geometry { time: view.time, pitch: view.pitch, ..inner };
    let color = colors::track_color(cx.theme, track.color);

    let response = ui.interact(geometry.grid, ui.scope_id().with("notes"), Sense::click_and_drag());
    paint_grid(ui, cx, &geometry, color, clip, midi);
    ruler(ui, cx, &geometry, clip, midi);
    keys(ui, cx, &geometry, track, color, is_drums);
    corner(ui, cx, &geometry);
    grid_interaction(ui, cx, &geometry, track, clip, &midi.notes, &response);
    if geometry.velocity.height() > 0.0 {
        let frame = controller_lane::Frame {
            header: geometry.lane_header,
            body: geometry.velocity,
            grid_left: geometry.grid.left(),
            time: geometry.time,
            snap: geometry.snap,
            cell: geometry.line_step(),
            signature,
            color,
            sounding: sounding_span(clip, midi),
            playhead: playhead_x(cx, &geometry, clip),
        };
        controller_lane::header(ui, cx, &frame, track, clip);
        // What the strip was just asked to show is shown in this frame.
        match cx.state.piano.lane {
            LaneShown::Velocity => velocity_lane(ui, cx, &geometry, color, clip, midi),
            LaneShown::Controller(kind) => controller_lane::show(ui, cx, &frame, clip, kind),
        }
    }
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

/// The fill of a note: the track's hue, more opaque the harder it is hit.
fn note_fill(color: Color32, velocity: u8) -> Color32 {
    fade(color, 0.45 + velocity as f32 / 230.0)
}

/// The part of the clip's content that plays, from and to: the loop if the
/// clip loops, else from where a trimmed clip starts in its content to
/// where it ends there.
fn sounding_span(clip: &Clip, midi: &MidiContent) -> (Beats, Beats) {
    match midi.loop_length {
        Some(length) => (0.0, length.min(clip.length)),
        None => (midi.start_offset.max(0.0), midi.start_offset.max(0.0) + clip.length),
    }
}

/// Shades what lies outside `span` in `area`: the part of the content
/// that does not play. Returns the x of the span's end.
pub(super) fn shade_outside(painter: &egui::Painter, area: Rect, from_x: f32, to_x: f32, shade: Color32) {
    if from_x > area.left() {
        painter.rect_filled(Rect::from_min_max(area.min, pos2(from_x.min(area.right()), area.bottom())), 0.0, shade);
    }
    if to_x < area.right() {
        painter.rect_filled(Rect::from_min_max(pos2(to_x.max(area.left()), area.top()), area.max), 0.0, shade);
    }
}

fn paint_grid(ui: &Ui, cx: &Cx, geometry: &Geometry, color: Color32, clip: &Clip, midi: &MidiContent) {
    let theme = cx.theme;
    let grid = geometry.grid;
    let painter = ui.painter_at(grid);
    painter.rect_filled(grid, 0.0, theme.bg);

    let (low, high) = geometry.visible_pitches();
    for pitch in low..=high {
        let top = geometry.y(pitch);
        if timeline::is_black_key(pitch) {
            let row = Rect::from_x_y_ranges(grid.x_range(), top..=top + geometry.pitch.row_height);
            painter.rect_filled(row, 0.0, theme.shade);
        }
        // Octaves are separated below each C; E/F, the other pair of
        // neighboring white keys, gets a fainter line.
        let separator = match pitch % 12 {
            0 => Some(theme.grid),
            5 => Some(theme.grid_fine),
            _ => None,
        };
        if let Some(color) = separator {
            paint::hline_above(&painter, grid.x_range(), (top + geometry.pitch.row_height).round(), color);
        }
    }

    let (first_beat, last_beat) = geometry.time.visible(grid.width());
    timeline::for_each_grid_line(first_beat, last_beat, geometry.meter, geometry.line_step(), |beat, level| {
        let color = match level {
            GridLevel::Bar => theme.hairline,
            GridLevel::Beat => theme.grid,
            GridLevel::Subdivision => theme.grid_fine,
        };
        paint::vline(&painter, geometry.x(beat).round(), grid.y_range(), color);
    });

    let (from, to) = sounding_span(clip, midi);
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
        // A pixel of the grid shows between neighbors and between rows.
        let body = Rect::from_min_max(rect.min + vec2(0.0, 1.0), rect.max - vec2(1.0, 0.0));
        let mut fill = note_fill(color, note.velocity);
        if note.start >= to || note.end() <= from {
            fill = fade(fill, 0.35);
        }
        bodies.add_colored_rect(body.intersect(grid), fill);
        if cx.state.selected_notes.contains(note) {
            outlines.push(body);
        }
    }
    painter.add(Shape::mesh(bodies));
    for outline in outlines {
        paint::frame(&painter, outline, theme.ink);
    }

    // Past the end of the clip (or of its loop) nothing new plays, and
    // nothing of what a trimmed clip hides before its start.
    let (start_x, end_x) = (geometry.x(from).round(), geometry.x(to).round());
    shade_outside(&painter, grid, start_x, end_x, theme.shade);
    if start_x > grid.left() {
        paint::vline(&painter, start_x, grid.y_range(), theme.hairline);
    }
    if end_x < grid.right() {
        paint::vline(
            &painter,
            end_x,
            grid.y_range(),
            if midi.loop_length.is_some() { theme.ink_faint } else { theme.hairline },
        );
    }

    if let Some(NoteDrag::Marquee { origin, .. }) = &cx.state.piano.drag
        && let Some(pointer) = ui.input(|input| input.pointer.latest_pos())
    {
        let band = Rect::from_two_pos(*origin, pointer);
        painter.rect_filled(band, 0.0, theme.selected);
        paint::frame(&painter, band, theme.ink_dim);
    }

    paint::vline(&painter, geometry.x(cx.state.piano.cursor).round(), grid.y_range(), theme.ink_faint);
    // The song's playhead, mapped into the clip (and around its loop).
    if let Some(x) = playhead_x(cx, geometry, clip) {
        paint::vline(&painter, x, grid.y_range(), theme.signal);
    }
}

/// Where the song's playhead falls in the clip's own time (around its
/// loop, and past what a trimmed clip hides), if it is inside the clip.
fn playhead_x(cx: &Cx, geometry: &Geometry, clip: &Clip) -> Option<f32> {
    clip.content_beat(cx.live.position).map(|beat| geometry.x(beat).round())
}

/// The roll's ruler. A click on it places the cursor that pasted notes go
/// to.
fn ruler(ui: &Ui, cx: &mut Cx, geometry: &Geometry, clip: &Clip, midi: &MidiContent) {
    let response = ui.interact(geometry.ruler, ui.scope_id().with("roll-ruler"), Sense::click());
    cx.hint(&response, "Click to place the cursor \u{00B7} Ctrl+V pastes notes there");
    if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        cx.state.focus = FocusArea::PianoRoll;
        let free = ui.input(|input| input.modifiers.alt);
        let step = if free { None } else { geometry.snap };
        cx.state.piano.cursor = timeline::snap_round(geometry.beat(pointer.x), step);
    }
    paint_ruler(ui, cx, geometry, clip, midi);
}

fn paint_ruler(ui: &Ui, cx: &Cx, geometry: &Geometry, clip: &Clip, midi: &MidiContent) {
    let theme = cx.theme;
    let ruler = geometry.ruler;
    let painter = ui.painter_at(ruler);
    painter.rect_filled(ruler, 0.0, theme.panel);
    paint::hline_above(&painter, ruler.x_range(), ruler.bottom(), theme.hairline);

    // Bars are counted from the start of the clip.
    let signature = geometry.signature;
    let (first_beat, last_beat) = geometry.time.visible(ruler.width());
    let label_step = timeline::grid_step(geometry.time.pixels_per_beat, signature, 52.0);
    let style = trill_noir::TextStyle { tracking: 0.4, ..theme.text.micro };
    timeline::for_each_grid_line(first_beat, last_beat, geometry.meter, label_step, |beat, level| {
        let x = geometry.x(beat).round();
        if level == GridLevel::Bar {
            paint::vline(&painter, x, ruler.y_range(), theme.grid);
        }
        let room = Rect::from_min_max(pos2(x + 4.0, ruler.top()), ruler.max);
        text::paint(
            &painter,
            room,
            Align2::LEFT_CENTER,
            style,
            &timeline::format_bar_beat(beat, geometry.meter),
            theme.ink_faint,
        );
    });

    // A line along the ruler's foot marks the part of the clip that plays.
    let (from, to) = sounding_span(clip, midi);
    let span = Rect::from_min_max(
        pos2(geometry.x(from).round(), ruler.bottom() - 2.0),
        pos2(geometry.x(to).round(), ruler.bottom()),
    );
    painter.rect_filled(span, 0.0, if midi.loop_length.is_some() { theme.ink_dim } else { theme.ghost });

    // The cursor: a notch on the ruler's foot, over its line in the grid.
    let x = geometry.x(cx.state.piano.cursor).round();
    if x >= ruler.left() && x <= ruler.right() {
        let tip = pos2(x + 0.5, ruler.bottom());
        let notch = vec![tip + vec2(-4.0, -5.0), tip + vec2(4.0, -5.0), tip];
        painter.add(Shape::convex_polygon(notch, theme.ink_dim, egui::Stroke::NONE));
    }
}

/// The keyboard down the left side. Press a key to hear it. The notes the
/// computer keyboard is playing on this track are lit as well, and while it
/// is on each key it plays carries its letter.
fn keys(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, track: &Track, color: Color32, is_drums: bool) {
    let theme = cx.theme;
    let keys = geometry.keys;
    let response = ui.interact(keys, ui.scope_id().with("keys"), Sense::click_and_drag());
    let held = cx.state.audition.map(|audition| audition.pitch);
    let hovered = response.hover_pos().map(|pointer| geometry.pitch_at(pointer.y));
    // A key that is something other than a note says what it is.
    let meaning = hovered.and_then(|pitch| match is_drums {
        true => drum_row_name(track, &cx.snap.samples, pitch).map(|name| format!("{name} ({})", pitch_name(pitch))),
        false => key_meaning(track, pitch),
    });
    match meaning {
        Some(meaning) => cx.hint(&response, format!("{meaning} \u{00B7} press to hear it")),
        None => cx.hint(&response, "Press a key to hear it \u{00B7} slide to play across keys"),
    }

    let painter = ui.painter_at(keys);
    let row_height = geometry.pitch.row_height;
    painter.rect_filled(keys, 0.0, if is_drums { theme.panel } else { theme.key_white });
    let (low, high) = geometry.visible_pitches();
    for pitch in low..=high {
        let row = Rect::from_min_size(pos2(keys.left(), geometry.y(pitch)), vec2(keys.width(), row_height));
        let named = row_height >= 10.0 && row.top() >= keys.top() - 0.5 && row.bottom() <= keys.bottom() + 0.5;
        let lit = held == Some(pitch) || cx.keyboard.is_sounding(track.id, pitch);
        let letter = super::keyboard::letter_on(cx.keyboard, theme, pitch, timeline::is_black_key(pitch), lit);
        let state = KeyState { lit, hovered: hovered == Some(pitch), named, letter: letter.filter(|_| named) };
        if is_drums {
            let name = drum_row_name(track, &cx.snap.samples, pitch);
            paint_drum_row(&painter, theme, row, name.as_deref(), state, color);
        } else {
            paint_key(&painter, theme, row, pitch, state, color);
        }
    }
    paint::vline_left_of(&painter, keys.right(), keys.y_range(), theme.hairline);

    // Sliding along the keyboard with the button down plays each key.
    if response.is_pointer_button_down_on()
        && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
    {
        cx.state.focus = FocusArea::PianoRoll;
        let pitch = geometry.pitch_at(pointer.y);
        if held != Some(pitch) {
            let velocity = cx.state.piano.note_velocity;
            cx.audition(track.id, pitch, velocity, None);
        }
    }
}

#[derive(Copy, Clone)]
struct KeyState {
    /// The key is sounding.
    lit: bool,
    hovered: bool,
    /// The row is tall enough for its name and wholly in view: a name cut
    /// in half by the ruler reads as a glitch.
    named: bool,
    /// The letter of the computer key that plays this one, and its ink.
    letter: Option<(&'static str, Color32)>,
}

fn paint_key(painter: &egui::Painter, theme: &Theme, row: Rect, pitch: u8, state: KeyState, color: Color32) {
    let black = timeline::is_black_key(pitch);
    let fill = match (state.lit, black) {
        (true, _) => Some(color),
        (false, true) => Some(theme.key_black),
        (false, false) => None,
    };
    if let Some(fill) = fill {
        painter.rect_filled(row, 0.0, fill);
    }
    if state.hovered && !state.lit {
        painter.rect_filled(row, 0.0, fade(if black { theme.key_white } else { theme.key_black }, 0.16));
    }
    // A line between every pair of keys, as the gaps of a real keyboard.
    paint::hline(painter, row.x_range(), row.top().round(), fade(theme.key_ink, 0.35));
    if pitch.is_multiple_of(12) && state.named {
        let ink = if state.lit { theme.on_accent } else { theme.key_ink };
        let label = Rect::from_min_max(row.min, pos2(row.right() - 4.0, row.bottom()));
        let style = trill_noir::TextStyle { tracking: 0.0, ..theme.text.micro };
        text::paint(painter, label, Align2::RIGHT_CENTER, style, &pitch_name(pitch), ink);
    }
    if let Some((letter, ink)) = state.letter {
        let style = trill_noir::TextStyle { tracking: 0.0, ..theme.text.micro };
        let place = Rect::from_min_max(pos2(row.left() + 5.0, row.top()), row.max);
        text::paint(painter, place, Align2::LEFT_CENTER, style, letter, ink);
    }
}

/// Drum tracks list their sounds by name instead of showing piano keys.
fn paint_drum_row(
    painter: &egui::Painter,
    theme: &Theme,
    row: Rect,
    name: Option<&str>,
    state: KeyState,
    color: Color32,
) {
    if state.lit {
        painter.rect_filled(row, 0.0, color);
    } else if state.hovered {
        painter.rect_filled(row, 0.0, theme.selected);
    } else if name.is_some() {
        painter.rect_filled(row, 0.0, theme.fill);
    }
    paint::hline(painter, row.x_range(), row.top().round(), theme.grid);
    if let Some(name) = name
        && state.named
    {
        let ink = match (state.lit, state.hovered) {
            (true, _) => theme.on_accent,
            (false, true) => theme.ink,
            (false, false) => theme.ink_dim,
        };
        let style = trill_noir::TextStyle { upper: false, tracking: 0.2, ..theme.text.micro };
        text::paint(painter, row.shrink2(vec2(6.0, 0.0)), Align2::LEFT_CENTER, style, name, ink);
    }
    if let Some((letter, _)) = state.letter {
        // At the far end from the drum's name, and quieter than it.
        let ink = if state.lit { theme.on_accent } else { theme.quiet_on(theme.panel) };
        let style = trill_noir::TextStyle { tracking: 0.0, ..theme.text.micro };
        text::paint(painter, row.shrink2(vec2(6.0, 0.0)), Align2::RIGHT_CENTER, style, letter, ink);
    }
}

/// Vertical zoom buttons above the keyboard.
fn corner(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let corner = geometry.corner;
    let painter = ui.painter_at(corner);
    painter.rect_filled(corner, 0.0, theme.panel);
    paint::hline_above(&painter, corner.x_range(), corner.bottom(), theme.hairline);
    paint::vline_left_of(&painter, corner.right(), corner.y_range(), theme.hairline);

    let button =
        |index: f32| Rect::from_min_size(pos2(corner.left() + 1.0 + index * 21.0, corner.top()), vec2(21.0, 17.0));
    let height = geometry.grid.height();
    let middle = geometry.grid.center().y;
    for (index, icon, factor, name) in
        [(0.0, Icon::Minus, 1.0 / 1.25, "Shorter rows"), (1.0, Icon::Plus, 1.25, "Taller rows")]
    {
        let id = ui.scope_id().with(("row-zoom", icon));
        let response = trill_noir::bare_icon_button(ui, theme, button(index), id, icon, name);
        cx.hint(&response, format!("{name} \u{00B7} Alt+scroll"));
        if response.clicked() {
            cx.state.piano.pitch.zoom_around(geometry.grid.top(), middle, factor, height);
        }
    }
}

// ---- the velocity lane -----------------------------------------------------

/// One stem per note along the bottom, as tall as the note is loud. Drag a
/// stem to change its note; selected notes follow.
fn velocity_lane(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, color: Color32, clip: &Clip, midi: &MidiContent) {
    let theme = cx.theme;
    let lane = geometry.velocity;
    let stems = lane.with_min_x(geometry.grid.left());
    let response = ui.interact(stems, ui.scope_id().with("velocity"), Sense::click_and_drag());

    let painter = ui.painter_at(lane);
    painter.rect_filled(lane, 0.0, theme.panel);
    paint::vline_left_of(&painter, geometry.grid.left(), lane.y_range(), theme.hairline);

    let painter = painter.with_clip_rect(stems);
    let (first_beat, last_beat) = geometry.time.visible(stems.width());
    let (from, to) = sounding_span(clip, midi);
    let foot = lane.bottom() - STEM_FOOT;
    // Selected stems are drawn last, so they are not hidden by a neighbor
    // that starts at the same time.
    let visible = midi.notes.iter().filter(|note| note.start >= first_beat - 1.0 && note.start <= last_beat);
    let (selected, plain): (Vec<&Note>, Vec<&Note>) = visible.partition(|note| cx.state.selected_notes.contains(note));
    for (notes, lit) in [(plain, false), (selected, true)] {
        for note in notes {
            let x = geometry.x(note.start).round();
            let top = geometry.stem_top(note.velocity).round();
            let ink = match (lit, note.start >= to || note.start < from) {
                (true, _) => theme.ink,
                (false, false) => color,
                (false, true) => fade(color, 0.35),
            };
            paint::vline(&painter, x, Rangef::new(top, foot), ink);
            painter.rect_filled(Rect::from_min_size(pos2(x - 1.0, top - 2.0), vec2(4.0, 4.0)), 0.0, ink);
        }
    }
    if let Some(x) = playhead_x(cx, geometry, clip) {
        paint::vline(&painter, x, lane.y_range(), theme.signal);
    }

    let hovered = response.hover_pos().and_then(|pointer| geometry.stem_at(&midi.notes, pointer.x));
    if cx.state.piano.drag.is_none() {
        if let Some(note) = hovered {
            ui.set_cursor_icon(CursorIcon::ResizeVertical);
            cx.state
                .hint
                .set(format!("Velocity {} \u{00B7} drag to change \u{00B7} selected notes follow", note.velocity));
        } else if response.hovered() {
            cx.state.hint.set("How hard each note is hit \u{00B7} drag a stem to change it");
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
        && let Some(grabbed) = geometry.stem_at(&midi.notes, press.x)
    {
        cx.state.focus = FocusArea::PianoRoll;
        if !cx.state.selected_notes.contains(&grabbed) {
            cx.state.selected_notes = vec![grabbed];
        }
        let originals = cx.state.selected_notes.clone();
        cx.begin_gesture();
        cx.state.piano.drag = Some(NoteDrag::Velocity { current: originals.clone(), originals, grabbed });
    }
    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(NoteDrag::Velocity { originals, current, grabbed }) = cx.state.piano.drag.clone()
    {
        ui.set_cursor_icon(CursorIcon::ResizeVertical);
        let change = geometry.velocity_at(pointer.y) as i32 - grabbed.velocity as i32;
        let target = revoiced_notes(&originals, change);
        if target != current && swap_notes(cx, clip.id, &current, &target) {
            cx.state.selected_notes = target.clone();
            cx.state.piano.drag = Some(NoteDrag::Velocity { originals, current: target, grabbed });
        }
    }
    // Also ends a drag whose button was released outside the window.
    if matches!(cx.state.piano.drag, Some(NoteDrag::Velocity { .. })) && !ui.input(|input| input.pointer.primary_down())
    {
        cx.state.piano.drag = None;
    }
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
    {
        match hit(pointer) {
            Some((_, SpanHit::End)) => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                cx.state.hint.set("Drag to change the note\u{2019}s length \u{00B7} Alt for no snapping");
            }
            Some(_) => cx
                .state
                .hint
                .set("Drag to move \u{00B7} Shift-click adds to the selection \u{00B7} right-click deletes"),
            None => {
                cx.state.hint.set("Click to add a note \u{00B7} drag to select \u{00B7} scroll moves through the keys")
            }
        }
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
                    velocity: cx.state.piano.note_velocity,
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
            // Belongs to the velocity lane.
            NoteDrag::Velocity { .. } => {}
        }
    }

    // Also ends a drag whose button was released outside the window.
    let in_grid = !matches!(cx.state.piano.drag, Some(NoteDrag::Velocity { .. }) | None);
    if in_grid && !ui.input(|input| input.pointer.primary_down()) {
        cx.state.piano.drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::Project;

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
    fn velocities_move_together_and_stay_in_range() {
        let soft = Note { velocity: 20, ..note(60, 0.0, 1.0) };
        let loud = Note { velocity: 120, ..note(64, 1.0, 1.0) };
        let velocities = |notes: Vec<Note>| notes.iter().map(|note| note.velocity).collect::<Vec<_>>();
        assert_eq!(velocities(revoiced_notes(&[soft, loud], 5)), [25, 125]);
        assert_eq!(velocities(revoiced_notes(&[soft, loud], 30)), [50, 127], "the loud note stops at the top");
        assert_eq!(velocities(revoiced_notes(&[soft, loud], -60)), [1, 60], "a note is never silent");
        assert_eq!(revoiced_notes(&[soft], 0), [soft]);
    }

    #[test]
    fn quantizing_moves_starts_onto_the_grid_and_keeps_lengths() {
        let loose = [note(60, 0.07, 0.4), note(62, 0.9, 1.0), note(64, 1.5, 0.25)];
        assert_eq!(quantized_notes(&loose, 0.25), [note(60, 0.0, 0.4), note(62, 1.0, 1.0), note(64, 1.5, 0.25)]);
        assert_eq!(quantized_notes(&loose, 1.0)[2].start, 2.0);
    }

    #[test]
    fn legato_closes_the_gaps_between_notes() {
        let line = [note(60, 0.0, 0.25), note(62, 1.0, 0.25), note(64, 3.0, 0.5)];
        assert_eq!(legato_notes(&line), [note(60, 0.0, 1.0), note(62, 1.0, 2.0), note(64, 3.0, 0.5)]);
        // A chord is held as one, until whatever comes after it.
        let chord = [note(60, 0.0, 0.25), note(64, 0.0, 0.5), note(67, 2.0, 1.0)];
        assert_eq!(legato_notes(&chord), [note(60, 0.0, 2.0), note(64, 0.0, 2.0), note(67, 2.0, 1.0)]);
        // Overlapping notes are shortened to meet the next one too.
        assert_eq!(legato_notes(&[note(60, 0.0, 3.0), note(62, 1.0, 1.0)])[0].length, 1.0);
        assert!(legato_notes(&[]).is_empty());
    }

    #[test]
    fn copied_notes_are_counted_from_their_first() {
        let chord = [note(60, 2.5, 1.0), note(64, 3.0, 0.5), note(67, 4.25, 1.0)];
        assert_eq!(rebased_notes(&chord), [note(60, 0.0, 1.0), note(64, 0.5, 0.5), note(67, 1.75, 1.0)]);
        assert!(rebased_notes(&[]).is_empty());
    }

    #[test]
    fn a_phrase_is_as_long_as_the_grid_steps_it_covers() {
        let chord = [note(60, 2.5, 1.0), note(64, 3.0, 0.5), note(67, 4.25, 1.0)];
        assert_eq!(phrase_length(&chord, None), 2.75);
        assert_eq!(phrase_length(&chord, Some(0.25)), 2.75);
        assert_eq!(phrase_length(&chord, Some(1.0)), 3.0, "rounded up, so repeats stay on the beat");
        assert_eq!(phrase_length(&[note(60, 1.0, 2.0)], Some(1.0)), 2.0, "an exact fit is not rounded further");
        assert_eq!(phrase_length(&[note(60, 1.0, 0.01)], Some(0.25)), 0.25);
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
    fn the_view_moves_no_further_than_it_takes_to_show_a_range() {
        // Twenty-four rows, C4 to F5 (60..=77) wanted.
        assert_eq!(revealed_top(84.0, 24.0, 60, 77), 84.0, "in view already (60 up to 84): left alone");
        assert_eq!(revealed_top(90.0, 24.0, 60, 77), 84.0, "too high: down until the lowest key is the bottom row");
        assert_eq!(revealed_top(70.0, 24.0, 60, 77), 78.0, "too low: up until the highest key is the top row");
        assert_eq!(revealed_top(78.0, 24.0, 60, 77), 78.0);
        // A view too short for the range shows it from the bottom.
        assert_eq!(revealed_top(100.0, 12.0, 60, 77), 72.0);
        assert_eq!(revealed_top(40.0, 12.0, 60, 77), 72.0);
    }

    #[test]
    fn drum_rows_are_named() {
        assert_eq!(kit_drum_name(36), Some("Kick"));
        assert_eq!(kit_drum_name(42), Some("Closed Hat"));
        assert_eq!(kit_drum_name(60), None);
    }

    #[test]
    fn a_drum_tracks_rows_take_their_names_from_its_map_then_from_general_midi() {
        let mut project = Project::new("Rows");
        let kit = project.add_track("Kit");
        let none = HashMap::new();
        let row = |project: &Project, pitch: u8| drum_row_name(project.track(kit).unwrap(), &none, pitch);
        // Nothing said: General MIDI, under each drum's first name.
        assert_eq!(row(&project, 36).as_deref(), Some("Kick"));
        assert_eq!(row(&project, 52).as_deref(), Some("China"));
        assert_eq!(row(&project, 22), None, "General MIDI has no drum there");

        // The track's own map: its names on its keys, and a drum it moved is not where it was.
        let mut profile = project.track(kit).unwrap().profile.clone();
        profile.sounds = [("closed_hat_edge", 22), ("china", 86)].map(|(name, key)| (name.to_string(), key)).into();
        project.set_track_profile(kit, profile).unwrap();
        assert_eq!(row(&project, 22).as_deref(), Some("Closed Hat Edge"));
        assert_eq!(row(&project, 86).as_deref(), Some("China"));
        assert_eq!(row(&project, 52), None, "the china is on another key now");
        assert_eq!(row(&project, 38).as_deref(), Some("Snare"), "the rest is as it was");

        // A drum rack's pad is called after the file on it.
        let chain = trill_core::ChainRef::Track(kit);
        let rack = project.add_device(chain, super::super::sample_panels::DRUM_RACK, "Drum Rack", None).unwrap();
        assert_eq!(drum_row_name(project.track(kit).unwrap(), &none, 48), None, "General MIDI has no drum on 48");
        let file = SampleFile { slot: "pad 37".into(), path: "C:/Kit/808 rim.wav".into(), problem: None };
        let samples = HashMap::from([(rack, vec![file])]);
        let row = |pitch: u8| drum_row_name(project.track(kit).unwrap(), &samples, pitch);
        assert_eq!(row(37).as_deref(), Some("808 rim"));
        assert_eq!(row(22).as_deref(), Some("Closed Hat Edge"), "what the track says comes first");

        // The built-in kit names what it plays, also where General MIDI has nothing, and no more.
        let own = project.add_track("Own kit");
        project.add_device(trill_core::ChainRef::Track(own), DRUM_KIT, "Drum Kit", None).unwrap();
        let row = |pitch: u8| drum_row_name(project.track(own).unwrap(), &none, pitch);
        assert_eq!((row(48).as_deref(), row(44).as_deref()), (Some("High Tom"), Some("Closed Hat")));
        assert_eq!(row(52), None, "it has no china");
    }

    #[test]
    fn a_pitched_tracks_keyswitches_noises_and_range_are_told_apart_from_its_notes() {
        let mut project = Project::new("Keys");
        let guitar = project.add_track("Guitar");
        assert_eq!(key_meaning(project.track(guitar).unwrap(), 26), None, "a track that knows nothing says nothing");
        let profile = trill_core::InstrumentProfile {
            articulations: vec![trill_core::Articulation::key("palm_mute", 26)],
            sounds: [("scrape".to_string(), 101)].into(),
            range: Some((40, 88)),
            ..Default::default()
        };
        project.set_track_profile(guitar, profile).unwrap();
        let meaning = |pitch: u8| key_meaning(project.track(guitar).unwrap(), pitch).unwrap_or_default();
        assert!(meaning(26).starts_with("D1 is a keyswitch (palm_mute)"), "{}", meaning(26));
        assert!(meaning(101).starts_with("F7 plays a noise (scrape)"), "{}", meaning(101));
        assert!(meaning(30).contains("outside what this instrument plays (E2-E6)"), "{}", meaning(30));
        assert_eq!(meaning(60), "", "a note like any other");
    }

    fn geometry() -> Geometry {
        // Twenty rows of notes between the ruler and the lane.
        let area =
            Rect::from_min_size(pos2(200.0, 500.0), vec2(800.0, RULER_HEIGHT + 240.0 + LANE_HEADER + VELOCITY_LANE));
        let pitch = PitchView { top: 72.0, row_height: 12.0 };
        Geometry::new(
            area,
            KEYS_WIDTH,
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
    fn the_lane_and_its_strip_sit_under_keys_and_grid() {
        let g = geometry();
        assert_eq!(g.lane_header.top(), g.grid.bottom());
        assert_eq!((g.lane_header.height(), g.velocity.top()), (LANE_HEADER, g.lane_header.bottom()));
        assert_eq!(g.velocity.height(), VELOCITY_LANE);
        assert_eq!((g.velocity.left(), g.velocity.right()), (g.keys.left(), g.grid.right()));
        assert_eq!((g.lane_header.left(), g.lane_header.right()), (g.keys.left(), g.grid.right()));
        // A panel dragged very short keeps the notes and drops the lane,
        // strip and all.
        let short = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 100.0));
        let cramped = Geometry { ..Geometry::new(short, KEYS_WIDTH, g.time, g.pitch, g.signature, Snap::Sixteenth) };
        assert_eq!((cramped.velocity.height(), cramped.lane_header.height()), (0.0, 0.0));
        assert_eq!(cramped.grid.bottom(), short.bottom());
    }

    #[test]
    fn stems_map_velocities_to_heights_and_back() {
        let g = geometry();
        assert_eq!(g.stem_top(127), g.velocity.top() + STEM_HEADROOM);
        assert!(g.stem_top(1) < g.velocity.bottom() - STEM_FOOT, "the quietest note still has a stem");
        for velocity in [1u8, 37, 64, 100, 127] {
            assert_eq!(g.velocity_at(g.stem_top(velocity)), velocity);
        }
        assert_eq!(g.velocity_at(g.velocity.top() - 500.0), 127);
        assert_eq!(g.velocity_at(g.velocity.bottom() + 500.0), 1);
    }

    #[test]
    fn the_nearest_stem_is_grabbed_and_the_loudest_of_a_chord() {
        let g = geometry();
        let soft = Note { velocity: 40, ..note(60, 1.0, 1.0) };
        let loud = Note { velocity: 110, ..note(64, 1.0, 1.0) };
        let later = note(67, 2.0, 1.0);
        let notes = [soft, loud, later];
        assert_eq!(g.stem_at(&notes, g.x(1.0) + 3.0), Some(loud), "of a chord, the stem whose head shows");
        assert_eq!(g.stem_at(&notes, g.x(2.0) - 5.0), Some(later));
        assert_eq!(g.stem_at(&notes, g.x(1.5)), None, "out of reach of both");
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

    #[test]
    fn louder_notes_are_more_opaque() {
        let color = Color32::from_rgb(222, 138, 164);
        assert!(note_fill(color, 127).a() > note_fill(color, 64).a());
        assert!(note_fill(color, 1).a() > 100, "the quietest note is still clearly there");
        assert_eq!(note_fill(color, 127).a(), 255);
    }
}
