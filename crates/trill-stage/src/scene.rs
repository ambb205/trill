//! Painting the stage: the song's interface as Trill shows it, enlarged
//! and composed for the camera.
//!
//! Everything is painted with the `trill-noir` kit, in its tokens and by
//! its rules (hairlines on the pixel grid, zero rounding, caps labels), in
//! the vocabulary of the window: lanes with clips that carry their name on
//! a strip of the track's hue and show their notes small, piano rolls with
//! keys down the side and black-key rows shaded, a ruler of bars and
//! markers, the playhead in the signal color.
//!
//! It is painted three times over, into the three layers of a
//! [`Canvas`]:
//!
//! * `base`: the interface.
//! * `light`: what emits light, on black: the notes that sound, the keys
//!   they hold down, the playhead, the meters. The passes spread this
//!   into a glow and add it; nothing else in the picture glows.
//! * `focus`: how far out of focus each part of the picture is, as a gray:
//!   black is sharp. Whatever the shot is not about is soft, and both
//!   panes soften away from the playhead.
//!
//! What a part's lane and meter show of its level comes from what was
//! heard, where its track was measured; where it was not, its notes stand
//! in (a part is as lit as the note it is playing).

use crate::camera::{Camera, Roll};
use crate::director::{Kind, Plan};
use crate::light;
use crate::score::{Part, Score};
use crate::sound::Heard;
use egui::{Align2, Color32, Mesh, Painter, Rangef, Rect, Shape, Stroke, Vec2, pos2, vec2};
use trill_core::{Beats, Note, TimeSignature};
use trill_noir::{TextStyle, Theme, fade, mark, paint, text};

/// The three layers a moment is painted into.
pub struct Canvas<'a> {
    pub base: Painter,
    pub light: Painter,
    pub focus: Painter,
    pub theme: &'a Theme,
}

/// What is painted: one moment of a song.
pub struct Moment<'a> {
    pub score: &'a Score,
    pub plan: &'a Plan,
    pub camera: &'a Camera,
    pub heard: &'a Heard,
}

/// The time axis of a 16:9 picture is this many points wide; the camera's
/// windows are meant for it.
const REFERENCE_AXIS: f32 = 1076.0;
/// A lane may be this tall however small the pane, and a third of the
/// pane where that is more.
const LANE_TALLEST: f32 = 84.0;
/// A lane this tall or taller gives its clips a name strip.
const LANE_WITH_STRIPS: f32 = 30.0;
const CLIP_STRIP: f32 = 14.0;
/// How much of its color a clip's name strip has away from the playhead,
/// and for how many bars behind the playhead it is lit by it.
const STRIP_AWAY: f32 = 0.42;
const STRIP_LIT_BARS: f32 = 1.5;
/// How much darker the lanes are when the piano rolls have the stage.
const LANES_VEIL: f32 = 0.62;
/// Height of a roll's title bar, and the gap between two rolls.
const ROLL_TITLE: f32 = 18.0;
const ROLL_GAP: f32 = 6.0;
/// Width of a piano roll's keys.
const KEYS: f32 = 54.0;
/// How soft the lanes are when the piano rolls have the stage, and how
/// soft either pane gets at its far ends, against the softest there is.
const LANES_SOFT: f32 = 0.7;
const FAR_SOFT: (f32, f32) = (0.1, 0.45);
/// How long a section's title stays up, in bars, and its type.
const TITLE_BARS: f64 = 2.0;
const TITLE: TextStyle = TextStyle { size: 44.0, tracking: 7.0, upper: true, tabular: false };

/// Where everything is, in points.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Layout {
    pub frame: Rect,
    /// The strip along the top: the mark, the song, where it is.
    pub top: Rect,
    /// Bars and markers, above the panes.
    pub ruler: Rect,
    /// The arrangement's lanes.
    pub lanes: Rect,
    /// The piano rolls; no height when the shot has none.
    pub rolls: Rect,
    /// The whole song as a line, along the bottom.
    pub line: Rect,
    /// Width of the column left of the time axis: lane names, keys.
    pub gutter: f32,
}

impl Layout {
    pub fn of(size: Vec2, roll_share: f32) -> Self {
        let wide = size.x >= size.y;
        let margin = if wide { 36.0 } else { 20.0 };
        let (top_height, line_height) = if wide { (50.0, 34.0) } else { (58.0, 40.0) };
        let frame = Rect::from_min_size(pos2(0.0, 0.0), size);
        let top = Rect::from_min_max(pos2(margin, 0.0), pos2(size.x - margin, top_height));
        let line = Rect::from_min_max(pos2(margin, size.y - 26.0 - line_height), pos2(size.x - margin, size.y - 26.0));
        let ruler = Rect::from_min_max(pos2(margin, top.bottom() + 6.0), pos2(size.x - margin, top.bottom() + 40.0));
        let panes = Rect::from_min_max(pos2(margin, ruler.bottom()), pos2(size.x - margin, line.top() - 16.0));
        let share = roll_share.clamp(0.0, 1.0);
        let gap = 8.0 * (share * 8.0).min(1.0);
        let rolls_height = (panes.height() * share).round();
        let lanes = Rect::from_min_max(panes.min, pos2(panes.right(), panes.bottom() - rolls_height - gap));
        let rolls = Rect::from_min_max(pos2(panes.left(), panes.bottom() - rolls_height), panes.max);
        Self { frame, top, ruler, lanes, rolls, line, gutter: if wide { 132.0 } else { 124.0 } }
    }

    /// The left and right ends of the time axis.
    pub fn axis(&self) -> Rangef {
        Rangef::new(self.ruler.left() + self.gutter, self.ruler.right())
    }

    /// How wide the time axis of a picture of `size` points is against a
    /// 16:9 one: what the camera scales its windows by.
    pub fn axis_ratio(size: Vec2) -> f64 {
        (Self::of(size, 0.0).axis().span() / REFERENCE_AXIS) as f64
    }

    /// The panes and the ruler above them.
    pub fn stage(&self) -> Rect {
        Rect::from_min_max(self.ruler.min, self.rolls.max)
    }
}

/// The time axis: which beat is where across the picture.
#[derive(Copy, Clone)]
struct Axis {
    left: f32,
    right: f32,
    origin: Beats,
    window: Beats,
}

impl Axis {
    fn x(&self, beat: Beats) -> f32 {
        self.left + ((beat - self.origin) / self.window) as f32 * (self.right - self.left)
    }

    fn per_beat(&self) -> f32 {
        (self.right - self.left) / self.window as f32
    }

    fn end(&self) -> Beats {
        self.origin + self.window
    }
}

/// Paints `moment` into `canvas`, in a picture of `size` points.
pub fn paint(canvas: &Canvas<'_>, moment: &Moment<'_>, size: Vec2) {
    let camera = moment.camera;
    let layout = Layout::of(size, camera.roll_share);
    let range = layout.axis();
    let axis = Axis { left: range.min, right: range.max, origin: camera.origin, window: camera.window };
    canvas.base.rect_filled(layout.frame, 0.0, canvas.theme.bg);

    // The focus first: what is painted after it may keep itself sharp.
    focus(canvas, moment, &layout, &axis);
    lanes(canvas, moment, &layout, &axis);
    rolls(canvas, moment, &layout, &axis);
    ruler(canvas, moment, &layout, &axis);
    playhead(canvas, moment, &layout, &axis);
    // The stage's own frame: a hairline round the ruler and the panes.
    paint::frame(&canvas.base, layout.stage(), canvas.theme.hairline);
    top_strip(canvas, moment, &layout);
    song_line(canvas, moment, &layout);
    title(canvas, moment, &layout, &axis);
}

/// How much the piano rolls have the stage, 0..=1: the lanes step back by
/// as much.
fn rolls_lead(camera: &Camera) -> f32 {
    ((camera.roll_share / 0.5).clamp(0.0, 1.0)) * (1.0 - camera.overview)
}

// ---- helpers ---------------------------------------------------------------

/// `from` on the way to `to`: `amount` 0 is `from`, 1 is `to`.
fn toward(from: Color32, to: Color32, amount: f32) -> Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Color32::from_rgba_premultiplied(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
        mix(from.a(), to.a()),
    )
}

/// The gray that says how soft a part of the picture is, 0..=1.
fn soft(amount: f32) -> Color32 {
    Color32::from_gray((amount.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The fill of a note in a piano roll: the track's hue, more opaque the
/// harder it is hit. The window's rule.
fn note_fill(color: Color32, velocity: u8) -> Color32 {
    fade(color, 0.45 + velocity as f32 / 230.0)
}

fn is_black_key(pitch: i32) -> bool {
    matches!(pitch.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// How lit a part is: what was heard of its track, or where that was not
/// measured, the brightest note it is playing.
fn part_level(moment: &Moment<'_>, part: &Part) -> f32 {
    if let Some(level) = moment.heard.track(part.track) {
        return level;
    }
    let (beat, tempo) = (moment.camera.beat, moment.score.tempo);
    let tail = light::GLOW_TAIL * tempo / 60.0;
    part.notes_between(beat - tail, beat + 1e-9).map(|note| light::glow(note, beat, tempo)).fold(0.0, f32::max)
}

/// `"25. 1. 1"`: bar, beat and sixteenth, as the window's transport reads.
fn position_text(beat: Beats, signature: TimeSignature) -> String {
    let beat = beat.max(0.0) + 1e-9;
    let per_bar = signature.beats_per_bar();
    let unit = 4.0 / signature.denominator as f64;
    let bar = (beat / per_bar).floor();
    let in_bar = beat - bar * per_bar;
    let count = (in_bar / unit).floor();
    let sixteenth = ((in_bar - count * unit) / 0.25).floor();
    format!("{}. {}. {}", bar as u32 + 1, count as u32 + 1, sixteenth as u32 + 1)
}

/// `"1:07.4"`.
fn clock_text(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).floor() as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

/// `"156"`, `"127.5"`: a tempo without the decimals it does not have.
fn tempo_text(tempo: f64) -> String {
    let text = format!("{tempo:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A name as a label: a drum's `closed_hat_edge` reads `closed hat edge`.
fn words(name: &str) -> String {
    name.replace('_', " ")
}

/// Vertical lines of the time grid in `rect`: bars, and beats and
/// sixteenths where there is room for them.
fn grid_lines(painter: &Painter, score: &Score, axis: &Axis, rect: Rect, colors: [Color32; 3], finest: f32) {
    let bar = score.bar();
    let per_bar = axis.per_beat() * bar as f32;
    // Bars, thinned out where they would crowd: every fourth, sixteenth…
    let mut every = 1.0;
    while per_bar * (every as f32) < 7.0 {
        every *= 4.0;
    }
    let step = if axis.per_beat() / 4.0 >= finest {
        0.25
    } else if axis.per_beat() >= finest {
        1.0
    } else {
        bar * every
    };
    let first = (axis.origin.max(0.0) / step).floor();
    let mut index = first;
    loop {
        let beat = index * step;
        if beat > axis.end().min(score.length) + 1e-9 {
            break;
        }
        index += 1.0;
        let x = axis.x(beat);
        if x < rect.left() - 1.0 || x > rect.right() + 1.0 {
            continue;
        }
        let on_bar = (beat / bar).fract().abs() < 1e-9;
        let on_beat = beat.fract().abs() < 1e-9;
        let color = match (on_bar, on_beat) {
            (true, _) if (beat / bar / every).fract().abs() < 1e-9 => colors[0],
            (true, _) => continue,
            (false, true) => colors[1],
            (false, false) => colors[2],
        };
        paint::vline(painter, x, rect.y_range(), color);
    }
}

// ---- the lanes -------------------------------------------------------------

fn lanes(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout, axis: &Axis) {
    let (theme, pane) = (canvas.theme, layout.lanes);
    if pane.height() < 2.0 {
        return;
    }
    let base = canvas.base.with_clip_rect(pane);
    let field = Rect::from_min_max(pos2(axis.left, pane.top()), pane.max);
    let gutter = Rect::from_min_max(pane.min, pos2(axis.left, pane.bottom()));
    base.rect_filled(gutter, 0.0, theme.panel);
    grid_lines(
        &base.with_clip_rect(field),
        moment.score,
        axis,
        field,
        [theme.grid, theme.grid_fine, theme.grid_fine],
        14.0,
    );

    let open: f32 = moment.camera.lanes.iter().map(|lane| lane.open).sum();
    // The lanes share the pane; a lane or two alone do not fill it.
    let unit = (pane.height() / open.max(1.0)).min((pane.height() / 3.0).max(LANE_TALLEST));
    let (mut notes, mut lit) = (Mesh::default(), Mesh::default());
    let mut top = pane.top();
    for (view, part) in moment.camera.lanes.iter().zip(&moment.score.parts) {
        let height = unit * view.open;
        let row = Rect::from_min_max(
            pos2(pane.left(), paint::snap(&base, top)),
            pos2(pane.right(), paint::snap(&base, top + height)),
        );
        top += height;
        if row.height() >= 1.0 {
            lane(canvas, moment, axis, row, part, view.open, &mut notes, &mut lit);
        }
    }
    base.with_clip_rect(field).add(Shape::mesh(notes));
    canvas.light.with_clip_rect(field).add(Shape::mesh(lit));
    paint::vline_left_of(&base, axis.left, pane.y_range(), theme.hairline);
    // Where the piano rolls have the stage the lanes step back: darker,
    // and their light with them.
    let veil = LANES_VEIL * rolls_lead(moment.camera);
    if veil > 0.0 {
        base.rect_filled(pane, 0.0, fade(theme.bg, veil));
        canvas.light.with_clip_rect(pane).rect_filled(pane, 0.0, Color32::from_black_alpha((veil * 255.0) as u8));
    }
}

#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn lane(
    canvas: &Canvas<'_>,
    moment: &Moment<'_>,
    axis: &Axis,
    row: Rect,
    part: &Part,
    open: f32,
    notes: &mut Mesh,
    lit: &mut Mesh,
) {
    let theme = canvas.theme;
    let (beat, tempo) = (moment.camera.beat, moment.score.tempo);
    let color = theme.stored_hue(part.color);
    let level = part_level(moment, part);
    let base = canvas.base.with_clip_rect(row);
    let light = canvas.light.with_clip_rect(row);
    let hair = paint::hair(&base);

    // The header: the track's hue down its edge, its name, its meter.
    let cell = Rect::from_min_max(row.min, pos2(axis.left, row.bottom()));
    base.rect_filled(Rect::from_min_max(cell.min, pos2(cell.left() + 3.0, cell.bottom())), 0.0, fade(color, open));
    let meter = Rect::from_min_max(
        pos2(cell.right() - 11.0, cell.top() + (cell.height() * 0.18).min(5.0)),
        pos2(cell.right() - 7.0, cell.bottom() - (cell.height() * 0.18).min(5.0)),
    );
    if cell.height() >= 9.0 {
        base.rect_filled(meter, 0.0, theme.well);
        let filled = Rect::from_min_max(pos2(meter.left(), meter.bottom() - meter.height() * level), meter.max);
        base.rect_filled(filled, 0.0, color);
        light.rect_filled(filled, 0.0, fade(color, 0.55));
    }
    if cell.height() >= 12.0 {
        let style = if cell.height() >= 20.0 { theme.text.caps } else { theme.text.micro };
        let ink = toward(theme.ink_faint, theme.ink, (level * 3.0).min(1.0));
        let room = Rect::from_min_max(pos2(cell.left() + 12.0, cell.top()), pos2(meter.left() - 6.0, cell.bottom()));
        text::paint(&base, room, Align2::LEFT_CENTER, style, &part.name, fade(ink, open));
    }
    paint::hline_above(&base, row.x_range(), row.bottom(), theme.hairline);

    // The clips.
    let field = Rect::from_min_max(pos2(axis.left, row.top()), row.max);
    let clipped = base.with_clip_rect(field);
    let strips = row.height() >= LANE_WITH_STRIPS;
    let strip = if strips { CLIP_STRIP } else { (row.height() * 0.16).clamp(hair, 2.0) };
    for clip in &part.clips {
        let (left, right) = (axis.x(clip.start), axis.x(clip.end) - hair);
        if right < field.left() || left > field.right() {
            continue;
        }
        let rect = Rect::from_min_max(pos2(left, row.top()), pos2(right.max(left + hair), row.bottom() - hair));
        let playing = (clip.start..clip.end).contains(&beat);
        let lift = if playing { level } else { 0.0 };
        clipped.rect_filled(rect, 0.0, fade(color, 0.13 + 0.1 * lift));
        // The name strip. In the window it is a solid block of the
        // track's color; at this size a lane of solid strips would be all
        // the eye sees, so here it steps back and is solid only where the
        // playhead has just been: lit by it, and cooling behind it.
        let title = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + strip));
        clipped.rect_filled(title, 0.0, fade(color, STRIP_AWAY));
        if playing {
            let x = axis.x(beat).min(title.right());
            let reach = (axis.per_beat() * moment.score.bar() as f32 * STRIP_LIT_BARS).max(40.0);
            let from = (x - reach).max(title.left());
            let most = (1.0 - (x - from) / reach).clamp(0.0, 1.0);
            let lit = 0.6 + 0.4 * lift;
            let (cool, hot) = (fade(color, lit * most), fade(color, lit));
            let mut glow = Mesh::default();
            glow.colored_vertex(pos2(from, title.top()), cool);
            glow.colored_vertex(pos2(x, title.top()), hot);
            glow.colored_vertex(pos2(x, title.bottom()), hot);
            glow.colored_vertex(pos2(from, title.bottom()), cool);
            glow.add_triangle(0, 1, 2);
            glow.add_triangle(0, 2, 3);
            clipped.add(Shape::mesh(glow));
            // Its top edge catches the light there.
            let edge =
                Rect::from_min_max(pos2((x - reach * 0.5).max(title.left()), title.top()), pos2(x, title.top() + hair));
            canvas.light.with_clip_rect(field).rect_filled(edge, 0.0, fade(color, 0.2 + 0.4 * lift));
        }
        if strips && rect.width() > 22.0 {
            // The name stays in view while the clip's start has gone by.
            let text_left = (rect.left().max(field.left()) + 4.0).min(rect.right() - 14.0);
            let room = Rect::from_min_max(pos2(text_left, title.top()), pos2(title.right() - 3.0, title.bottom()));
            text::paint(&clipped, room, Align2::LEFT_CENTER, theme.text.micro, &clip.name, theme.on_accent);
        }
    }

    // The notes, small: one line per pitch (or per drum).
    let content = Rect::from_min_max(pos2(field.left(), row.top() + strip), pos2(field.right(), row.bottom() - hair));
    let inset = (content.height() * 0.14).min(4.0);
    let area = content.shrink2(vec2(0.0, inset));
    if area.height() < 1.0 {
        return;
    }
    let rows = if part.drums { part.rows.len().max(1) as f32 } else { (part.high - part.low).max(11) as f32 + 1.0 };
    let line = (area.height() / rows).clamp(hair, (row.height() * 0.05).max(5.0));
    let tail = light::GLOW_TAIL * tempo / 60.0;
    for note in part.notes_between(axis.origin, axis.end()) {
        let step = match part.drums {
            true => part.row_of(note.pitch).unwrap_or(0) as f32,
            false => (note.pitch - part.low) as f32,
        };
        let y = area.bottom() - line - step / rows * (area.height() - line);
        let (left, right) = (axis.x(note.start), axis.x(note.end()));
        let bar = Rect::from_min_max(pos2(left, y), pos2(right.max(left + 1.5) - 0.5, y + line)).intersect(content);
        if bar.width() <= 0.0 {
            continue;
        }
        let glow = if note.start <= beat && beat < note.end() + tail { light::glow(note, beat, tempo) } else { 0.0 };
        let ink = if glow > 0.01 {
            toward(color, Color32::WHITE, glow * 0.7)
        } else if note.end() <= beat {
            fade(color, 0.5)
        } else {
            color
        };
        notes.add_colored_rect(bar, ink);
        if glow > 0.01 {
            lit.add_colored_rect(bar.expand(0.5), fade(color, glow));
        }
    }
}

// ---- the piano rolls -------------------------------------------------------

fn rolls(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout, axis: &Axis) {
    let pane = layout.rolls;
    let total: f32 = moment.camera.rolls.iter().map(|roll| roll.weight).sum();
    if pane.height() < 4.0 || total <= 0.0 {
        return;
    }
    let mut top = pane.top();
    for view in &moment.camera.rolls {
        let height = (pane.height() + ROLL_GAP) * view.weight / total - ROLL_GAP;
        let rect = Rect::from_min_max(
            pos2(pane.left(), paint::snap(&canvas.base, top)),
            pos2(pane.right(), paint::snap(&canvas.base, top + height.max(0.0))),
        );
        top += height.max(0.0) + ROLL_GAP;
        if let Some(part) = moment.score.part(view.track).filter(|_| rect.height() >= ROLL_TITLE + 12.0) {
            roll(canvas, moment, axis, rect, part, view);
        }
    }
}

/// What one roll needs to know of its part at this moment.
struct Sounding {
    /// The pitches that glow, and how much.
    pitches: Vec<(u8, f32)>,
    /// The note that started last among those sounding.
    latest: Option<Note>,
}

fn sounding(part: &Part, beat: Beats, tempo: f64) -> Sounding {
    let tail = light::GLOW_TAIL * tempo / 60.0;
    let mut pitches: Vec<(u8, f32)> = Vec::new();
    let mut latest: Option<Note> = None;
    for note in part.notes_between(beat - tail, beat + 1e-9) {
        let glow = light::glow(note, beat, tempo);
        if glow <= 0.01 {
            continue;
        }
        match pitches.iter_mut().find(|(pitch, _)| *pitch == note.pitch) {
            Some((_, most)) => *most = most.max(glow),
            None => pitches.push((note.pitch, glow)),
        }
        if note.end() > beat && latest.is_none_or(|other| note.start >= other.start) {
            latest = Some(*note);
        }
    }
    Sounding { pitches, latest }
}

/// Where the rows of a roll are: a semitone each, or a drum each.
enum RowsAt<'a> {
    Pitched {
        /// The pitch at the bottom edge, and the height of a semitone.
        low: f32,
        height: f32,
        bottom: f32,
    },
    /// The drums on stage, lowest first: pitch, name, top edge, height.
    Drums(Vec<(u8, &'a str, f32, f32)>),
}

impl<'a> RowsAt<'a> {
    fn of(part: &'a Part, view: &Roll, grid: Rect) -> Self {
        if !part.drums {
            let height = grid.height() / (view.high - view.low).max(1.0);
            return Self::Pitched { low: view.low, height, bottom: grid.bottom() };
        }
        // The drums of the take, each as open as the camera has it; where
        // the take has none, the whole kit.
        let chosen: Vec<(u8, f32)> = match view.drums.is_empty() {
            true => part.rows.iter().map(|row| (row.pitch, 1.0)).collect(),
            false => view.drums.clone(),
        };
        let total: f32 = chosen.iter().map(|(_, open)| open).sum::<f32>().max(1e-3);
        let unit = grid.height() / total;
        let mut bottom = grid.bottom();
        let mut rows = Vec::with_capacity(chosen.len());
        for (pitch, open) in chosen {
            let height = unit * open;
            let name = part.rows.iter().find(|row| row.pitch == pitch).map_or("", |row| row.name.as_str());
            rows.push((pitch, name, bottom - height, height));
            bottom -= height;
        }
        Self::Drums(rows)
    }

    /// The top edge and the height of the row `pitch` is on, if it has one.
    fn row(&self, pitch: u8) -> Option<(f32, f32)> {
        match self {
            Self::Pitched { low, height, bottom } => Some((bottom - (pitch as f32 + 1.0 - low) * height, *height)),
            Self::Drums(rows) => rows.iter().find(|row| row.0 == pitch).map(|row| (row.2, row.3)),
        }
    }
}

fn roll(canvas: &Canvas<'_>, moment: &Moment<'_>, axis: &Axis, rect: Rect, part: &Part, view: &Roll) {
    let theme = canvas.theme;
    let (beat, tempo) = (moment.camera.beat, moment.score.tempo);
    let color = theme.stored_hue(part.color);
    let base = canvas.base.with_clip_rect(rect);
    let hair = paint::hair(&base);
    let now = sounding(part, beat, tempo);
    base.rect_filled(rect, 0.0, theme.bg);

    // The title bar: whose roll this is, and the clip it is in.
    let bar = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + ROLL_TITLE));
    base.rect_filled(bar, 0.0, theme.panel);
    paint::hline_above(&base, bar.x_range(), bar.bottom(), theme.hairline);
    let after_dot = paint::dot(&base, pos2(bar.left() + 10.0, bar.center().y), theme.metric.dot, color);
    canvas.light.with_clip_rect(bar).rect_filled(
        Rect::from_min_max(pos2(bar.left() + 10.0, bar.center().y - 2.5), pos2(after_dot, bar.center().y + 2.5)),
        0.0,
        fade(color, part_level(moment, part)),
    );
    let name_room = Rect::from_min_max(pos2(after_dot + 7.0, bar.top()), bar.max);
    let used = text::paint(&base, name_room, Align2::LEFT_CENTER, theme.text.caps, &part.name, theme.ink);
    if let Some(clip) = part.clips.iter().find(|clip| (clip.start..clip.end).contains(&beat)) {
        let room = Rect::from_min_max(pos2(name_room.left() + used + 10.0, bar.top()), bar.max);
        text::paint(&base, room, Align2::LEFT_CENTER, theme.text.micro, &clip.name, theme.ink_faint);
    }
    // At the far end, the note being played.
    if let Some(note) = now.latest {
        let label = match part.drums {
            true => words(part.rows.iter().find(|row| row.pitch == note.pitch).map_or("", |row| row.name.as_str())),
            false => trill_core::pitch::pitch_name(note.pitch),
        };
        let room = Rect::from_min_max(bar.min, pos2(bar.right() - 10.0, bar.bottom()));
        text::paint(&base, room, Align2::RIGHT_CENTER, theme.text.value, &label, theme.ink_dim);
    }

    let body = Rect::from_min_max(pos2(rect.left(), bar.bottom()), rect.max);
    let grid = Rect::from_min_max(pos2(axis.left, body.top()), body.max);
    let side = Rect::from_min_max(body.min, pos2(axis.left, body.bottom()));
    // The title bar and the side are read, not looked through.
    canvas.focus.rect_filled(bar, 0.0, soft(0.0));
    canvas.focus.rect_filled(side, 0.0, soft(0.0));
    let field = base.with_clip_rect(grid);
    let lit_field = canvas.light.with_clip_rect(grid);
    let rows = RowsAt::of(part, view, grid);
    let glow_of = |pitch: u8| now.pitches.iter().find(|(found, _)| *found == pitch).map_or(0.0, |(_, glow)| *glow);

    match &rows {
        RowsAt::Drums(rows) => {
            for (index, (_, _, top, height)) in rows.iter().enumerate() {
                if index % 2 == 1 {
                    field.rect_filled(Rect::from_x_y_ranges(grid.x_range(), *top..=top + height), 0.0, theme.grid_fine);
                }
                paint::hline(&field, grid.x_range(), *top, theme.grid_fine);
            }
        }
        RowsAt::Pitched { height, .. } => {
            for pitch in view.low.floor() as i32..=view.high.ceil() as i32 {
                let Some((top, _)) = u8::try_from(pitch).ok().and_then(|pitch| rows.row(pitch)) else { continue };
                if is_black_key(pitch) {
                    field.rect_filled(Rect::from_x_y_ranges(grid.x_range(), top..=top + height), 0.0, theme.shade);
                }
                // Octaves are separated below each C; E and F, the other
                // pair of neighboring white keys, get a fainter line.
                let separator = match pitch.rem_euclid(12) {
                    0 => Some(theme.grid),
                    5 => Some(theme.grid_fine),
                    _ => None,
                };
                if let Some(line) = separator {
                    paint::hline_above(&field, grid.x_range(), top + height, line);
                }
            }
        }
    }
    grid_lines(&field, moment.score, axis, grid, [theme.hairline, theme.grid, theme.grid_fine], 9.0);

    // The notes.
    let (mut bodies, mut lit) = (Mesh::default(), Mesh::default());
    let tail = light::GLOW_TAIL * tempo / 60.0;
    for note in part.notes_between(axis.origin, axis.end()) {
        let Some((top, row_height)) = rows.row(note.pitch) else { continue };
        if top > grid.bottom() || top + row_height < grid.top() || row_height < 0.5 {
            continue;
        }
        let left = axis.x(note.start);
        // A drum is a hit, not a length: it is as wide as it is high at
        // least.
        let shortest = if part.drums { (row_height * 0.8).clamp(3.0, 14.0) } else { 2.0 };
        let right = axis.x(note.end()).max(left + shortest);
        // A pixel of the grid shows between neighbors and between rows.
        let body = Rect::from_min_max(pos2(left, top + hair), pos2(right - hair, top + row_height));
        let fill = note_fill(color, note.velocity);
        let glow = if note.start <= beat && beat < note.end() + tail { light::glow(note, beat, tempo) } else { 0.0 };
        let ink = if glow > 0.01 {
            toward(toward(fill, color, glow * 1.6), Color32::WHITE, (glow - 0.45) * 0.9)
        } else if note.end() <= beat {
            fade(fill, 0.5)
        } else {
            fill
        };
        bodies.add_colored_rect(body, ink);
        if glow > 0.01 {
            lit.add_colored_rect(body, fade(color, glow));
        }
    }
    field.add(Shape::mesh(bodies));
    lit_field.add(Shape::mesh(lit));

    // The side: keys, or the names of the drums.
    let side_base = base.with_clip_rect(side);
    let side_light = canvas.light.with_clip_rect(side);
    side_base.rect_filled(side, 0.0, theme.panel);
    match &rows {
        RowsAt::Drums(rows) => {
            let style = TextStyle { upper: false, tracking: 0.2, ..theme.text.micro };
            for (pitch, name, top, height) in rows {
                let row = Rect::from_min_size(pos2(side.left(), *top), vec2(side.width(), *height));
                let glow = glow_of(*pitch);
                side_base.rect_filled(row, 0.0, toward(theme.fill, color, glow * 1.4));
                if glow > 0.01 {
                    side_light.rect_filled(row, 0.0, fade(color, glow * 0.8));
                }
                paint::hline(&side_base, row.x_range(), row.top(), theme.grid);
                if *height >= theme.text.micro.size + 1.0 {
                    let ink = toward(theme.ink_dim, theme.on_accent, glow * 2.0);
                    text::paint(
                        &side_base,
                        row.shrink2(vec2(12.0, 0.0)),
                        Align2::LEFT_CENTER,
                        style,
                        &words(name),
                        ink,
                    );
                }
            }
        }
        RowsAt::Pitched { height, .. } => {
            let row_height = *height;
            let keys = Rect::from_min_max(pos2(side.right() - KEYS, side.top()), side.max);
            let keys_base = base.with_clip_rect(keys);
            keys_base.rect_filled(keys, 0.0, theme.key_white);
            for pitch in view.low.floor() as i32..=view.high.ceil() as i32 {
                let Some((top, _)) = u8::try_from(pitch).ok().and_then(|pitch| rows.row(pitch)) else { continue };
                let row = Rect::from_min_size(pos2(keys.left(), top), vec2(keys.width(), row_height));
                let glow = glow_of(pitch as u8);
                if is_black_key(pitch) {
                    keys_base.rect_filled(row, 0.0, theme.key_black);
                }
                if glow > 0.01 {
                    keys_base.rect_filled(row, 0.0, fade(color, (glow * 2.0).min(1.0)));
                    canvas.light.with_clip_rect(keys).rect_filled(row, 0.0, fade(color, glow * 0.8));
                }
                // A line between every pair of keys, as the gaps of a real
                // keyboard.
                paint::hline(&keys_base, row.x_range(), row.top(), fade(theme.key_ink, 0.35));
                if pitch.rem_euclid(12) == 0 && row_height >= theme.text.micro.size {
                    let style = TextStyle { tracking: 0.0, ..theme.text.micro };
                    let label = Rect::from_min_max(row.min, pos2(row.right() - 5.0, row.bottom()));
                    let name = trill_core::pitch::pitch_name(pitch as u8);
                    text::paint(&keys_base, label, Align2::RIGHT_CENTER, style, &name, theme.key_ink);
                }
            }
            // Beside the keys, the name of each note as it sounds: it goes
            // where the tune goes.
            for (pitch, glow) in &now.pitches {
                let Some((top, _)) = rows.row(*pitch) else { continue };
                let tall = row_height.max(theme.text.value.size + 2.0);
                let label = Rect::from_min_max(
                    pos2(side.left() + 8.0, top + row_height / 2.0 - tall / 2.0),
                    pos2(keys.left() - 9.0, top + row_height / 2.0 + tall / 2.0),
                );
                let name = trill_core::pitch::pitch_name(*pitch);
                let ink = fade(toward(color, theme.ink, 0.35), (glow * 2.2).min(1.0));
                text::paint(&side_base, label, Align2::RIGHT_CENTER, theme.text.value, &name, ink);
                text::paint(&side_light, label, Align2::RIGHT_CENTER, theme.text.value, &name, fade(color, glow * 0.6));
            }
        }
    }
    paint::vline_left_of(&base, axis.left, body.y_range(), theme.hairline);
}

// ---- the ruler and the playhead -------------------------------------------

fn ruler(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout, axis: &Axis) {
    let (theme, rect) = (canvas.theme, layout.ruler);
    let (score, camera) = (moment.score, moment.camera);
    let base = canvas.base.with_clip_rect(rect);
    base.rect_filled(rect, 0.0, theme.panel);
    paint::hline_above(&base, rect.x_range(), rect.bottom(), theme.hairline);

    // What the camera is doing, where the window has its corner buttons.
    let shot = match moment.plan.takes.get(camera.take).map(|take| take.shot.kind) {
        Some(Kind::Overview) => "Song",
        Some(Kind::Wide) => "Wide",
        Some(Kind::Ensemble) => "Ensemble",
        Some(Kind::Close) => "Close",
        None => "",
    };
    let corner = Rect::from_min_max(pos2(rect.left() + 12.0, rect.top()), pos2(axis.left - 8.0, rect.bottom()));
    text::paint(&base, corner, Align2::LEFT_CENTER, theme.text.micro, shot, theme.ink_faint);
    paint::vline_left_of(&base, axis.left, rect.y_range(), theme.hairline);

    let time = Rect::from_min_max(pos2(axis.left, rect.top()), rect.max);
    let clipped = base.with_clip_rect(time);
    let split = rect.top() + 16.0;
    // Markers, on the upper row: a flag and the section's name.
    let now = score.section_at(camera.beat);
    let style = theme.text.micro;
    for (index, section) in score.sections.iter().enumerate() {
        let (left, next) = (axis.x(section.start), axis.x(section.end));
        if next < time.left() || left > time.right() || section.name.is_empty() {
            continue;
        }
        let ink = if index == now { theme.ink } else { theme.ink_dim };
        paint::vline(&clipped, left, rect.y_range(), theme.hairline);
        // The name of the section the song is in stays in view; a name
        // that does not fit before the next marker is left to its flag.
        let width = text::width(&clipped, style, &section.name);
        let text_left = (left.max(time.left()) + 12.0).min(next - width - 6.0);
        let fits = text_left >= left + 12.0 - 0.5;
        let flag = pos2(if fits { text_left } else { left + 12.0 } - 8.0, rect.top() + 4.5);
        clipped.add(Shape::convex_polygon(vec![flag, flag + vec2(5.5, 3.5), flag + vec2(0.0, 7.0)], ink, Stroke::NONE));
        if fits {
            let room = Rect::from_min_max(pos2(text_left, rect.top()), pos2(time.right(), split));
            text::paint(&clipped, room, Align2::LEFT_CENTER, style, &section.name, ink);
        }
    }
    // Bars, on the lower: numbered as often as there is room.
    let bar = score.bar();
    let per_bar = axis.per_beat() * bar as f32;
    let mut every = 1.0f64;
    while per_bar * (every as f32) < 46.0 {
        every *= 2.0;
    }
    let numbers = TextStyle { tracking: 0.4, ..theme.text.micro };
    let mut index = (axis.origin.max(0.0) / bar / every).floor();
    loop {
        let beat = index * every * bar;
        if beat >= score.length || beat > axis.end() {
            break;
        }
        index += 1.0;
        let x = axis.x(beat);
        if x < time.left() - 40.0 {
            continue;
        }
        paint::vline(&clipped, x, Rangef::new(split, rect.bottom()), theme.grid);
        let room = Rect::from_min_max(pos2(x + 4.0, split), pos2(x + 60.0, rect.bottom()));
        let number = format!("{}", (beat / bar).round() as u64 + 1);
        text::paint(&clipped, room, Align2::LEFT_CENTER, numbers, &number, theme.ink_faint);
    }
}

fn playhead(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout, axis: &Axis) {
    let theme = canvas.theme;
    let stage = layout.stage();
    let base = canvas.base.with_clip_rect(stage);
    let light = canvas.light.with_clip_rect(stage);
    let x = paint::snap(&base, axis.x(moment.camera.beat));
    if x < axis.left - 1.0 || x > axis.right + 1.0 {
        return;
    }
    let width = 2.0 * paint::hair(&base);
    // Where the lanes have stepped back the line steps back with them:
    // it is as far away as they are.
    let away = rolls_lead(moment.camera);
    let lanes = layout.lanes;
    for (span, strength) in [
        (Rangef::new(stage.top(), lanes.top()), 1.0),
        (lanes.y_range(), 1.0 - 0.6 * away),
        (Rangef::new(lanes.bottom(), stage.bottom()), 1.0),
    ] {
        let line = Rect::from_x_y_ranges(Rangef::new(x, x + width), span);
        base.rect_filled(line, 0.0, fade(theme.signal, strength));
        light.rect_filled(line, 0.0, fade(theme.signal, strength * strength));
    }
    // Its head, on the ruler's foot.
    let tip = pos2(x + width / 2.0, layout.ruler.bottom());
    let head = vec![tip + vec2(-5.0, -7.0), tip + vec2(5.0, -7.0), tip];
    base.add(Shape::convex_polygon(head.clone(), theme.signal, Stroke::NONE));
    light.add(Shape::convex_polygon(head, theme.signal, Stroke::NONE));
    // Light falls from it onto what it has just passed, more with the
    // sound: a wash that is strongest at the line.
    let reach = 16.0 + 22.0 * moment.heard.punch;
    let strength = 0.015 + 0.055 * moment.heard.punch;
    let panes = Rect::from_min_max(pos2(axis.left, layout.ruler.bottom()), stage.max);
    let mut wash = Mesh::default();
    let (near, far) = (fade(theme.signal, strength), fade(theme.signal, 0.0));
    for (from, to) in [(x - reach, x), (x + width + reach * 0.3, x + width)] {
        let first = wash.vertices.len() as u32;
        wash.colored_vertex(pos2(from, panes.top()), far);
        wash.colored_vertex(pos2(to, panes.top()), near);
        wash.colored_vertex(pos2(to, panes.bottom()), near);
        wash.colored_vertex(pos2(from, panes.bottom()), far);
        wash.add_triangle(first, first + 1, first + 2);
        wash.add_triangle(first, first + 2, first + 3);
    }
    canvas.light.with_clip_rect(panes).add(Shape::mesh(wash));
}

// ---- the strips above and below -------------------------------------------

fn top_strip(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout) {
    let (theme, rect) = (canvas.theme, layout.top);
    let (score, camera) = (moment.score, moment.camera);
    let base = &canvas.base;
    let middle = rect.center().y + 3.0;
    // The mark, on whole pixels at any size.
    let scale = base.pixels_per_point();
    let unit = (1.3 * scale).round().max(1.0) / scale;
    let mut x = rect.left();
    x += mark::paint_centered(base, x, middle, unit).width() + 9.0;
    let row = |x: f32| Rect::from_min_max(pos2(x, rect.top() + 6.0), pos2(rect.right(), rect.bottom()));
    x += text::paint(base, row(x), Align2::LEFT_CENTER, theme.text.brand, "Trill", theme.ink) + 14.0;
    paint::vline(base, x, Rangef::new(middle - 7.0, middle + 7.0), theme.hairline);
    x += 14.0;
    text::paint(base, row(x), Align2::LEFT_CENTER, theme.text.body, &score.name, theme.ink_dim);

    // Where the song is, at the other end: bar and beat, the clock, the tempo.
    let mut right = rect.right();
    let cell =
        |right: f32, width: f32| Rect::from_min_max(pos2(right - width, rect.top() + 6.0), pos2(right, rect.bottom()));
    let tempo = format!("{} BPM", tempo_text(score.tempo));
    right -=
        text::paint(base, cell(right, 120.0), Align2::RIGHT_CENTER, theme.text.label, &tempo, theme.ink_faint) + 18.0;
    let clock = clock_text(score.seconds(camera.beat));
    text::paint(base, cell(right, 64.0), Align2::RIGHT_CENTER, theme.text.value, &clock, theme.ink_dim);
    right -= 64.0 + 14.0;
    let position = position_text(camera.beat, score.signature);
    text::paint(base, cell(right, 90.0), Align2::RIGHT_CENTER, theme.text.readout, &position, theme.ink);
}

fn song_line(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout) {
    let (theme, rect) = (canvas.theme, layout.line);
    let (score, camera) = (moment.score, moment.camera);
    let base = canvas.base.with_clip_rect(rect.expand(4.0));
    let y = paint::snap(&base, rect.bottom() - 7.0);
    let x_of = |beat: Beats| rect.left() + (beat / score.length.max(1e-9)) as f32 * rect.width();
    let head = x_of(camera.beat);
    let hair = paint::hair(&base);
    paint::hline(&base, rect.x_range(), y, theme.hairline);
    // What has been played, a pixel thicker and in ink.
    base.rect_filled(Rect::from_min_max(pos2(rect.left(), y - hair), pos2(head, y + hair)), 0.0, theme.ink_dim);
    let now = score.section_at(camera.beat);
    for (index, section) in score.sections.iter().enumerate() {
        let (left, next) = (x_of(section.start), x_of(section.end));
        let ink = if index == now { theme.ink } else { theme.ink_faint };
        paint::vline(
            &base,
            left,
            Rangef::new(y - 5.0, y + hair),
            if index <= now { theme.ink_dim } else { theme.ghost },
        );
        let room = Rect::from_min_max(pos2(left + 5.0, rect.top()), pos2(next - 4.0, y - 5.0));
        // Only the names that fit between their mark and the next.
        if text::width(&base, theme.text.micro, &section.name) <= room.width() {
            text::paint(&base, room, Align2::LEFT_BOTTOM, theme.text.micro, &section.name, ink);
        }
    }
    let dot = Rect::from_center_size(pos2(paint::snap(&base, head), y), Vec2::splat(theme.metric.dot + 1.0));
    base.rect_filled(dot, 0.0, theme.signal);
    canvas.light.with_clip_rect(rect.expand(4.0)).rect_filled(dot, 0.0, theme.signal);
}

/// The section's name, large, as it begins; at the song's first beat, the
/// song's.
fn title(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout, axis: &Axis) {
    let theme = canvas.theme;
    let (score, camera) = (moment.score, moment.camera);
    let index = score.section_at(camera.beat);
    let Some(section) = score.sections.get(index) else { return };
    let (name, sub) = match index {
        0 => (score.name.clone(), format!("{} bars at {} BPM", score.length / score.bar(), tempo_text(score.tempo))),
        _ => (section.name.clone(), format!("Bar {}", (section.start / score.bar()).round() as u64 + 1)),
    };
    if name.is_empty() {
        return;
    }
    let lasts = (TITLE_BARS * score.bar()).min(section.length());
    let since = camera.beat - section.start;
    if !(0.0..lasts).contains(&since) {
        return;
    }
    // Up at once, on the beat; away over the last of its time.
    let shown = ((lasts - since) / (lasts * 0.35)).clamp(0.0, 1.0) as f32;
    let shown = shown * shown * (3.0 - 2.0 * shown);
    let arriving = (-(since * 60.0 / score.tempo) / 0.25).exp() as f32;

    let panes = Rect::from_min_max(pos2(layout.ruler.left(), layout.ruler.bottom()), layout.rolls.max);
    let wide = layout.frame.width() >= layout.frame.height();
    let style = if wide { TITLE } else { TextStyle { size: 34.0, tracking: 5.0, ..TITLE } };
    let left = axis.left + 26.0;
    let room = Rect::from_min_max(
        pos2(left, panes.bottom() - style.size - 34.0),
        pos2(panes.right() - 20.0, panes.bottom() - 22.0),
    );
    let width = text::width(&canvas.base, style, &name).min(room.width());
    // A scrim under it, so that it reads over whatever plays behind:
    // darkest where the words are, gone a little beyond them.
    let scrim = Rect::from_min_max(
        pos2(axis.left, room.top() - 44.0),
        pos2((left + width + 150.0).min(panes.right()), panes.bottom()),
    );
    let (dark, clear) = (fade(theme.bg, 0.9 * shown), fade(theme.bg, 0.0));
    let mut mesh = Mesh::default();
    let corners = [
        (scrim.left_top(), clear),
        (pos2(left + width, scrim.top()), clear),
        (scrim.right_top(), clear),
        (pos2(scrim.left(), room.top() - 6.0), dark),
        (pos2(left + width, room.top() - 6.0), dark),
        (pos2(scrim.right(), room.top() - 6.0), clear),
        (scrim.left_bottom(), dark),
        (pos2(left + width, scrim.bottom()), dark),
        (scrim.right_bottom(), clear),
    ];
    for (position, color) in corners {
        mesh.colored_vertex(position, color);
    }
    for (a, b, c, d) in [(0, 1, 4, 3), (1, 2, 5, 4), (3, 4, 7, 6), (4, 5, 8, 7)] {
        mesh.add_triangle(a, b, c);
        mesh.add_triangle(a, c, d);
    }
    let clipped = canvas.base.with_clip_rect(panes);
    clipped.add(Shape::mesh(mesh));
    text::paint(&clipped, room, Align2::LEFT_BOTTOM, style, &name, fade(theme.ink, shown));
    let above = Rect::from_min_max(pos2(left + 2.0, room.top() - 22.0), pos2(room.right(), room.top() - 4.0));
    text::paint(&clipped, above, Align2::LEFT_BOTTOM, theme.text.caps, &sub, fade(theme.ink_dim, shown));
    // It arrives lit.
    if arriving > 0.02 {
        let lit = canvas.light.with_clip_rect(panes);
        text::paint(&lit, room, Align2::LEFT_BOTTOM, style, &name, fade(theme.ink, 0.5 * arriving));
    }
    let words = Rect::from_min_max(pos2(axis.left, room.top() - 30.0), pos2(left + width + 40.0, panes.bottom()));
    canvas.focus.rect_filled(words, 0.0, soft(0.0));
}

// ---- the focus -------------------------------------------------------------

/// Paints how soft each part of the picture is. Nothing is soft in the
/// overview; otherwise the lanes are when the rolls have the stage, and
/// both panes soften away from the playhead, more ahead of it than behind.
fn focus(canvas: &Canvas<'_>, moment: &Moment<'_>, layout: &Layout, axis: &Axis) {
    let camera = moment.camera;
    let near = 1.0 - camera.overview;
    if near <= 0.0 {
        return;
    }
    let x = axis.x(camera.beat);
    let span = axis.right - axis.left;
    for (pane, level) in [(layout.lanes, LANES_SOFT * rolls_lead(camera)), (layout.rolls, 0.0)] {
        if pane.height() < 2.0 {
            continue;
        }
        // The header column is as soft as its pane; the time axis is
        // sharp about the playhead and softens to either side of it, more
        // ahead than behind.
        let column = Rect::from_min_max(pane.min, pos2(axis.left, pane.bottom()));
        canvas.focus.rect_filled(column, 0.0, soft(level));
        let stops = [
            (axis.left, level + FAR_SOFT.0 * near),
            (x - span * 0.2, level),
            (x + span * 0.24, level),
            (axis.right, level + FAR_SOFT.1 * near),
        ];
        let mut mesh = Mesh::default();
        for (stop, amount) in stops {
            let stop = stop.clamp(axis.left, axis.right);
            mesh.colored_vertex(pos2(stop, pane.top()), soft(amount));
            mesh.colored_vertex(pos2(stop, pane.bottom()), soft(amount));
        }
        for column in 0..stops.len() as u32 - 1 {
            let first = column * 2;
            mesh.add_triangle(first, first + 2, first + 3);
            mesh.add_triangle(first, first + 3, first + 1);
        }
        canvas.focus.with_clip_rect(pane).add(Shape::mesh(mesh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panes_share_the_stage_by_the_shot() {
        let size = vec2(1280.0, 720.0);
        let wide = Layout::of(size, 0.0);
        assert_eq!(wide.rolls.height(), 0.0);
        assert_eq!(wide.lanes.bottom(), wide.rolls.bottom(), "no gap where there are no rolls");
        let close = Layout::of(size, 0.76);
        let panes = close.rolls.bottom() - close.lanes.top();
        assert!((close.rolls.height() / panes - 0.76).abs() < 0.01);
        assert!((close.rolls.top() - close.lanes.bottom() - 8.0).abs() < 1e-3, "a gap between the two");
        // The time axis is the same for both, so the playhead is one line.
        assert_eq!(close.axis(), wide.axis());
        assert_eq!(close.axis().span(), REFERENCE_AXIS);
        assert_eq!(Layout::axis_ratio(size), 1.0);
    }

    #[test]
    fn an_upright_picture_holds_fewer_beats_at_the_same_scale() {
        let upright = vec2(720.0, 1280.0);
        let ratio = Layout::axis_ratio(upright);
        assert!(ratio > 0.5 && ratio < 0.6, "{ratio}");
        let layout = Layout::of(upright, 0.6);
        assert!(layout.stage().height() > 1000.0, "and gives the height to the panes");
        assert!(layout.line.bottom() < 1280.0 && layout.top.top() == 0.0);
    }

    #[test]
    fn positions_read_as_the_transport_does() {
        let four = TimeSignature::default();
        assert_eq!(position_text(0.0, four), "1. 1. 1");
        assert_eq!(position_text(96.0, four), "25. 1. 1");
        assert_eq!(position_text(97.75, four), "25. 2. 4");
        assert_eq!(position_text(3.0, TimeSignature::new(6, 8).unwrap()), "2. 1. 1");
        assert_eq!(clock_text(0.0), "0:00.0");
        assert_eq!(clock_text(67.46), "1:07.4");
    }

    #[test]
    fn colors_meet_halfway() {
        let (black, white) = (Color32::BLACK, Color32::WHITE);
        assert_eq!(toward(black, white, 0.0), black);
        assert_eq!(toward(black, white, 1.0), white);
        assert_eq!(toward(black, white, 0.5), Color32::from_gray(128));
        assert_eq!(toward(black, white, 7.0), white, "never past it");
        assert_eq!(soft(0.5), Color32::from_gray(128));
        assert!(is_black_key(61) && !is_black_key(60) && is_black_key(-2));
    }
}
