//! How a clip is drawn, and which of its parts is where: the name strip,
//! the body, the two edges that trim it and, on audio clips, the two fade
//! handles.
//!
//! A MIDI clip shows the notes it actually plays ([`Clip::timeline_notes`],
//! through the snapshot's cache), so a trimmed or looped clip looks the way
//! it sounds. An audio clip shows the waveform of the stretch of its file
//! it plays: where the file is entered, how fast it is run through and
//! where it ends all show. The waveform comes from a worker
//! ([`crate::ui::sounds`]); until it has, and for a file that cannot be
//! read, the clip is hatched instead.

use super::Geometry;
use super::rows::RowKind;
use crate::ui::snapshot::Snapshot;
use crate::ui::sounds::WaveState;
use crate::ui::state::Cx;
use crate::ui::timeline::{self, EdgeHit};
use crate::ui::units;
use eframe::egui::{Align2, Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use std::sync::Arc;
use trill_core::project::{AudioContent, ClipContent, MidiContent};
use trill_core::{Beats, Clip, ClipId, TempoMap, Track, TrackId};
use trill_noir::{Theme, fade, paint, text};

/// Height of a clip's name strip.
pub(super) const CLIP_TITLE: f32 = 14.0;
/// Width of the zone at each end of a clip that trims it.
const TRIM_HANDLE: f32 = 6.0;
/// Height of the band under the name strip in which fade handles are
/// grabbed, and how far to either side of one the pointer may be.
const FADE_BAND: f32 = 11.0;
const FADE_GRAB: f32 = 5.0;
/// Side of a fade handle.
const FADE_HANDLE: f32 = 5.0;
/// Points along a fade's curve.
const FADE_STEPS: usize = 14;

/// The part of a clip under the pointer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ClipPart {
    /// The name strip: drags like the body, and a double-click renames.
    Title,
    Body,
    Start,
    End,
    FadeIn,
    FadeOut,
}

/// What the pointer is over in the lanes.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct ClipHit {
    pub track: TrackId,
    pub clip: ClipId,
    pub part: ClipPart,
    pub start: Beats,
}

/// The clip at `position`, topmost first, and which part of it.
pub(super) fn clip_at(tracks: &[Track], geometry: &Geometry, position: Pos2) -> Option<ClipHit> {
    let row = geometry.row_at(position.y)?;
    let RowKind::Track { index, .. } = row.kind else { return None };
    let track = &tracks[index];
    // Later clips are painted over earlier ones, so they are hit first.
    track.clips.iter().rev().find_map(|clip| {
        let edge = timeline::edge_hit(position.x, geometry.x(clip.start), geometry.x(clip.end()), TRIM_HANDLE)?;
        let rect = geometry.clip_rect(row, clip);
        let part = part_at(geometry, rect, clip, edge, position);
        Some(ClipHit { track: track.id, clip: clip.id, part, start: clip.start })
    })
}

fn part_at(geometry: &Geometry, rect: Rect, clip: &Clip, edge: EdgeHit, position: Pos2) -> ClipPart {
    let title_bottom = rect.top() + CLIP_TITLE;
    let in_fade_band = position.y >= title_bottom && position.y < title_bottom + FADE_BAND;
    if let Some(audio) = clip.audio().filter(|_| in_fade_band) {
        let (fade_in, fade_out) = fade_handles(geometry, clip, audio);
        let (to_in, to_out) = ((position.x - fade_in).abs(), (position.x - fade_out).abs());
        // Where a short clip brings the handles together, the nearer wins.
        if to_in <= FADE_GRAB && to_in <= to_out {
            return ClipPart::FadeIn;
        }
        if to_out <= FADE_GRAB {
            return ClipPart::FadeOut;
        }
    }
    match edge {
        EdgeHit::Start => ClipPart::Start,
        EdgeHit::End => ClipPart::End,
        EdgeHit::Body if position.y < title_bottom => ClipPart::Title,
        EdgeHit::Body => ClipPart::Body,
    }
}

/// Screen x of an audio clip's two fade handles: where the fade in ends
/// and where the fade out begins.
fn fade_handles(geometry: &Geometry, clip: &Clip, audio: &AudioContent) -> (f32, f32) {
    (geometry.x(clip.start + audio.fade_in), geometry.x(clip.end() - audio.fade_out))
}

/// The name strip of a clip painted in `rect`.
pub(super) fn title_rect(rect: Rect) -> Rect {
    Rect::from_min_max(rect.min, pos2(rect.right(), (rect.top() + CLIP_TITLE).min(rect.bottom())))
}

/// How a clip is colored.
#[derive(Copy, Clone)]
pub(super) struct ClipLook {
    /// The hue of its track.
    pub color: Color32,
    /// Muted, by itself or with its track: it keeps its hue but steps back.
    pub ghosted: bool,
    /// Screen x from which the clip is not covered by another: where its
    /// name may start.
    pub name_from: f32,
    /// Selected or under the pointer: an audio clip shows its fade handles.
    pub handles: bool,
    /// The name is being typed over the strip, so it is not painted.
    pub renaming: bool,
}

/// Paints one clip. Note miniatures go into `notes`, one mesh for the whole
/// arrangement: thousands of tiny rectangles cost one draw call instead of
/// one shape each.
pub(super) fn paint_clip(
    painter: &Painter,
    cx: &Cx,
    notes: &mut Mesh,
    rect: Rect,
    geometry: &Geometry,
    clip: &Clip,
    look: ClipLook,
) {
    let (theme, snapshot) = (cx.theme, cx.snap);
    let ClipLook { color, ghosted, name_from, .. } = look;
    let lanes = geometry.lanes;
    painter.rect_filled(rect, 0.0, fade(color, if ghosted { 0.06 } else { 0.15 }));

    // The name strip: the one solid block of the track's color.
    let title = title_rect(rect);
    let (strip, name) = if ghosted { (fade(color, 0.22), theme.ink_dim) } else { (color, theme.on_accent) };
    painter.rect_filled(title, 0.0, strip);
    if rect.width() > 18.0 && !look.renaming {
        // Keep the name on screen while the clip's start is scrolled away.
        let text_left = (rect.left().max(lanes.left()).max(name_from) + 4.0).min(rect.right() - 14.0);
        let room = Rect::from_min_max(pos2(text_left, title.top()), pos2(title.right() - 3.0, title.bottom()));
        text::paint(painter, room, Align2::LEFT_CENTER, theme.text.micro, &clip.name, name);
    }

    let content = Rect::from_min_max(pos2(rect.left(), title.bottom()), rect.max).intersect(lanes);
    if content.width() <= 0.0 || content.height() <= 2.0 {
        return;
    }
    let ink = if ghosted { fade(color, 0.4) } else { color };
    match &clip.content {
        ClipContent::Midi(midi) => paint_notes(painter, theme, snapshot, notes, content, geometry, clip, midi, ink),
        ClipContent::Audio(audio) => {
            paint_audio(painter, cx, content, rect, geometry, clip, audio, ink);
            paint_fades(painter, theme, geometry, rect, clip, audio, look);
        }
    }
}

/// Where in its file, in seconds, an audio clip is at timeline position
/// `beat`, in a song whose tempo is `tempo`. The clip enters the file at
/// its offset, and from there every beat covers the stretch of the
/// recording it covers (see [`AudioContent`]): a straight line for a clip
/// that follows the tempo and for a song of one tempo, and for a clip at
/// its own speed a line that bends where the tempo changes under it. This
/// is what puts a waveform under a clip: trimming the clip's start moves
/// the offset with it, so the picture stays put, and warping changes how
/// much of the file a beat covers.
pub(in crate::ui) fn file_second(clip: &Clip, audio: &AudioContent, tempo: &TempoMap, beat: Beats) -> f64 {
    audio.second_at(tempo, clip.start, beat)
}

/// Where the line of [`file_second`] bends between the timeline positions
/// `from` and `to`: nowhere for most clips. A glide is taken a beat at a
/// time, which no eye tells from the curve.
fn bends(audio: &AudioContent, tempo: &TempoMap, from: Beats, to: Beats) -> Vec<Beats> {
    let mut bends = Vec::new();
    if audio.followed_tempo().is_some() || tempo.is_constant() {
        return bends;
    }
    let mut beat = from.max(0.0);
    while bends.len() < 4096 {
        let change = tempo.next_change(beat).unwrap_or(f64::INFINITY);
        let next = if tempo.is_ramping_at(beat) { change.min(beat + 1.0) } else { change };
        if next >= to {
            break;
        }
        bends.push(next);
        beat = next;
    }
    bends
}

/// The length `clip` may take when it is asked to be `wanted` beats long.
///
/// A MIDI clip takes what it is asked, and any clip may get shorter. An
/// audio clip stops growing where its file ends: beyond that there is
/// nothing to play. One that is already longer (the tempo changed under
/// it) may stay so, but not grow. While the file is still being read
/// nobody knows where it ends, so the clip does not grow at all until that
/// is known; asking here is what has it read. A file that cannot be read
/// has no end to stop at, and its clip is silent at any length.
pub(in crate::ui) fn allowed_length(cx: &Cx, clip: &Clip, wanted: Beats) -> Beats {
    let Some(audio) = clip.audio().filter(|_| wanted > clip.length) else { return wanted };
    match cx.sounds.wave(&audio.path) {
        WaveState::Ready(wave) => {
            let seconds = (wave.seconds() - audio.offset_seconds).max(0.0);
            let room = audio.beats_over(&cx.snap.project.tempo_map(), clip.start, seconds);
            wanted.min(room.max(clip.length))
        }
        WaveState::Loading => clip.length,
        WaveState::Failed(_) => wanted,
    }
}

/// Draws what an audio clip plays: the waveform of its stretch of the
/// file, once that has been read, and until then the hatching.
#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn paint_audio(
    painter: &Painter,
    cx: &Cx,
    content: Rect,
    clip_rect: Rect,
    geometry: &Geometry,
    clip: &Clip,
    audio: &AudioContent,
    ink: Color32,
) {
    let theme = cx.theme;
    let name = audio.path.rsplit(['/', '\\']).next().unwrap_or(&audio.path);
    let wave = match cx.sounds.wave(&audio.path) {
        WaveState::Ready(wave) => wave,
        WaveState::Loading => {
            paint_audio_body(painter, theme, content, clip_rect, None, ink, theme.ink_dim);
            return;
        }
        WaveState::Failed(_) => {
            // The clip is silent; it says which file it is waiting for.
            let label = format!("Missing \u{00B7} {name}");
            paint_audio_body(painter, theme, content, clip_rect, Some(&label), fade(theme.warn, 0.7), theme.warn);
            return;
        }
    };
    let tempo = &cx.snap.project.tempo_map();
    let area = content.shrink2(vec2(0.0, 2.0f32.min(content.height() / 6.0)));
    let mut mesh = Mesh::default();
    let gain = 10f32.powf(audio.gain_db / 20.0);
    // Drawn in the stretches through which a beat covers the same length
    // of the recording: one, unless the tempo changes under a clip that
    // keeps its own speed.
    // And in the rounds of a clip that plays round: each begins at the start of its loop again.
    let (from_beat, to_beat) = (geometry.beat(content.left()), geometry.beat(content.right()));
    let rounds = audio.rounds(tempo, clip.start, from_beat, to_beat);
    let mut inside = bends(audio, tempo, from_beat, to_beat);
    inside.extend(rounds.iter().copied());
    inside.sort_by(f64::total_cmp);
    let mut edges = vec![content.left()];
    edges.extend(inside.into_iter().map(|beat| geometry.x(beat)));
    edges.push(content.right());
    for edge in edges.windows(2).filter(|edge| edge[1] > edge[0]) {
        let piece = Rect::from_x_y_ranges(edge[0]..=edge[1], area.y_range());
        let columns = trill_noir::waveform_columns(painter, piece.width());
        // From where the stretch begins in the recording, for as much of it as the stretch covers.
        let (first, last) = (geometry.beat(edge[0]), geometry.beat(edge[1]));
        let begins = file_second(clip, audio, tempo, first);
        let found = wave.columns(begins..begins + audio.seconds_over(tempo, first, last - first), columns);
        let bands: Vec<(f32, f32)> = found.iter().map(|column| (column.mixed.min, column.mixed.max)).collect();
        trill_noir::waveform(&mut mesh, piece, &bands, gain, fade(ink, 0.9), paint::hair(painter));
    }
    painter.with_clip_rect(content).add(Shape::mesh(Arc::new(mesh)));

    // Where a clip that plays round comes round, a hairline, as a notes clip has where its loop starts over.
    for beat in &rounds {
        let x = geometry.x(*beat).round();
        if x > content.left() && x < content.right() {
            paint::vline(painter, x, content.y_range(), fade(theme.ink_faint, 0.8));
        }
    }
    // Where the recording runs out before the clip does, a mark.
    let end = clip.start + audio.beats_over(tempo, clip.start, wave.seconds() - audio.offset_seconds);
    let x = geometry.x(end).round();
    if audio.looped.is_none() && end < clip.end() - 1e-6 && x > content.left() && x < content.right() {
        paint::vline(painter, x, content.y_range(), theme.ink_faint);
    }
    if cx.snap.clip_problems.contains_key(&clip.id) {
        // It plays, but not as asked (it could not be stretched).
        paint::corner_mark(painter, clip_rect, theme.warn);
    }
}

/// Draws what a MIDI clip plays as thin lines, with a hairline wherever its
/// loop starts over.
#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn paint_notes(
    painter: &Painter,
    theme: &Theme,
    snapshot: &Snapshot,
    mesh: &mut Mesh,
    content: Rect,
    geometry: &Geometry,
    clip: &Clip,
    midi: &MidiContent,
    ink: Color32,
) {
    let playing = snapshot.clip_notes(clip);
    let Some((low, high)) = playing.range else { return };
    // At least an octave of range, so a single note is not drawn as a slab.
    let span = (high - low).max(11) as f32 + 1.0;
    let area = content.shrink2(vec2(0.0, 4.0f32.min(content.height() / 4.0)));
    let line = (area.height() / span).clamp(1.0, 2.0);
    let y_of = |pitch: u8| (area.bottom() - line - (pitch - low) as f32 / span * (area.height() - line)).round();

    for start in loop_starts(clip, midi, geometry.time.pixels_per_beat) {
        let x = geometry.x(start).round();
        if x > content.left() && x < content.right() {
            paint::vline(painter, x, content.y_range(), theme.hairline);
        }
    }

    let (first_beat, last_beat) = (geometry.beat(content.left()), geometry.beat(content.right()));
    // Sorted by start: everything before the first note that could still
    // reach into view is skipped without looking at it.
    let longest = (clip.length).max(0.0);
    let from = playing.notes.partition_point(|note| note.start < first_beat - longest);
    for note in &playing.notes[from..] {
        if note.start > last_beat {
            break;
        }
        if note.end() < first_beat {
            continue;
        }
        let left = geometry.x(note.start);
        let right = geometry.x(note.end()).max(left + 1.5);
        let top = y_of(note.pitch);
        let bar = Rect::from_min_max(pos2(left, top), pos2(right - 0.5, top + line)).intersect(content);
        if bar.width() > 0.0 {
            mesh.add_colored_rect(bar, ink);
        }
    }
}

/// Timeline positions inside `clip` where its loop starts over. Empty for a
/// clip that does not loop, and for a loop only a few pixels wide, whose
/// marks would be a solid block.
fn loop_starts(clip: &Clip, midi: &MidiContent, pixels_per_beat: f64) -> Vec<Beats> {
    let Some(period) = midi.loop_length.filter(|length| *length > 0.0 && *length < clip.length) else {
        return Vec::new();
    };
    if period * pixels_per_beat < 3.0 {
        return Vec::new();
    }
    // The first repeat begins where the phrase, entered `start_offset`
    // beats in, comes round to its start.
    let first = period - midi.start_offset.rem_euclid(period);
    let count = ((clip.length - first) / period).ceil().max(0.0) as usize;
    (0..count).map(|repeat| clip.start + first + repeat as f64 * period).filter(|start| *start < clip.end()).collect()
}

/// An audio clip whose waveform cannot be drawn (yet): a hatched body with
/// a center line, and `label` on a plate when there is something to say.
fn paint_audio_body(
    painter: &Painter,
    theme: &Theme,
    content: Rect,
    clip_rect: Rect,
    label: Option<&str>,
    ink: Color32,
    label_ink: Color32,
) {
    const HATCH: f32 = 8.0;
    let clipped = painter.with_clip_rect(content);
    let hatch = Stroke::new(1.0, fade(ink, 0.3));
    let slant = content.height();
    // The hatching is anchored to the clip, not the view, so it does not
    // crawl while scrolling.
    let skipped = ((content.left() - clip_rect.left()) / HATCH).floor().max(0.0);
    let mut x = clip_rect.left() - slant + skipped * HATCH;
    while x < content.right() {
        clipped.line_segment([pos2(x, content.bottom()), pos2(x + slant, content.top())], hatch);
        x += HATCH;
    }
    let middle = content.center().y.round();
    paint::hline(&clipped, content.x_range(), middle, fade(ink, 0.8));

    if let Some(name) = label.filter(|_| content.width() > 48.0) {
        let room = Rect::from_min_max(pos2(content.left() + 4.0, middle + 2.0), content.max - vec2(4.0, 2.0));
        if room.height() >= theme.text.micro.size {
            // A plate under the name keeps the hatching from running
            // through the letters.
            // A file name keeps its case: it is a name, not a label.
            let file_style = trill_noir::TextStyle { upper: false, tracking: 0.2, ..theme.text.micro };
            let width = text::width(&clipped, file_style, name).min(room.width());
            let plate = Rect::from_min_size(room.min - vec2(2.0, 0.0), vec2(width + 4.0, room.height()));
            clipped.rect_filled(plate, 0.0, theme.bg);
            text::paint(&clipped, room, Align2::LEFT_CENTER, file_style, name, label_ink);
        }
    }
}

/// How loud a fade is `progress` (0..=1) of the way through, as a height:
/// the curve the handles draw. The engine ramps the amplitude in a straight
/// line; drawn on the scale of the faders and meters, where equal steps
/// sound about equal, that is a curve that rises fast and then levels off.
pub(in crate::ui) fn fade_level(progress: f32) -> f32 {
    units::db_to_fader(units::gain_to_db(progress.clamp(0.0, 1.0))) / units::db_to_fader(0.0)
}

/// The fades of an audio clip: each a curve from silence to full level,
/// with what it takes away shaded, and a handle where it meets full level.
fn paint_fades(
    painter: &Painter,
    theme: &Theme,
    geometry: &Geometry,
    rect: Rect,
    clip: &Clip,
    audio: &AudioContent,
    look: ClipLook,
) {
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + CLIP_TITLE), rect.max);
    if body.height() < 8.0 {
        return;
    }
    let clipped = painter.with_clip_rect(body.intersect(geometry.lanes));
    let (fade_in, fade_out) = fade_handles(geometry, clip, audio);
    let line = Stroke::new(1.0, if look.ghosted { theme.ink_faint } else { theme.ink });
    // Each fade as (the x where it is silent, the x where it is at full
    // level): the curve runs between them.
    for (silent, full) in [(rect.left(), fade_in), (rect.right(), fade_out)] {
        if (full - silent).abs() < 1.0 {
            continue;
        }
        let point = |progress: f32| {
            pos2(silent + (full - silent) * progress, body.bottom() - body.height() * fade_level(progress))
        };
        let curve: Vec<Pos2> = (0..=FADE_STEPS).map(|step| point(step as f32 / FADE_STEPS as f32)).collect();
        // What the fade removes: the corner above the curve.
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
        clipped.add(Shape::mesh(shade));
        clipped.add(Shape::line(curve, line));
    }
    if look.handles && rect.width() >= 3.0 * FADE_HANDLE {
        for x in [fade_in, fade_out] {
            // Kept inside the clip, so a handle at a corner is whole.
            let left = (x - FADE_HANDLE / 2.0).round().clamp(rect.left(), rect.right() - FADE_HANDLE);
            let handle = Rect::from_min_size(pos2(left, body.top()), vec2(FADE_HANDLE, FADE_HANDLE));
            clipped.rect_filled(handle, 0.0, theme.ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::DetailTab;
    use crate::ui::arrangement::TRACK_HEIGHT;
    use crate::ui::arrangement::rows::Rows;
    use crate::ui::state::UiState;
    use crate::ui::timeline::TimeView;
    use trill_core::{Note, Project};

    fn geometry(project: &Project) -> Geometry {
        let mut view = UiState::new(DetailTab::Clip).arrangement;
        view.time = TimeView { origin: 0.0, pixels_per_beat: 20.0 };
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 500.0));
        Geometry::new(area, &view, Rows::build(&project.tracks, &view), project.meter_map())
    }

    fn project_with_clips() -> (Project, TrackId, ClipId, ClipId) {
        let mut project = Project::new("t");
        let _first = project.add_track("A");
        let second = project.add_track("B");
        let early = project.add_midi_clip(second, "early", 4.0, 4.0).unwrap();
        let late = project.add_midi_clip(second, "late", 6.0, 8.0).unwrap();
        (project, second, early, late)
    }

    #[test]
    fn clips_are_hit_on_their_own_track_topmost_first() {
        let (project, track, early, late) = project_with_clips();
        let g = geometry(&project);
        let row = g.lanes.top() + TRACK_HEIGHT + 30.0;

        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(5.0), g.lanes.top() + 30.0)), None, "track A is empty");
        let hit = clip_at(&project.tracks, &g, pos2(g.x(5.0), row)).unwrap();
        assert_eq!((hit.track, hit.clip, hit.part, hit.start), (track, early, ClipPart::Body, 4.0));
        // Where the two overlap, the later clip is on top.
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(7.0), row)).unwrap().clip, late);
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(15.0), row)), None);
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(5.0), g.lanes.top() + 5.0 * TRACK_HEIGHT)), None);
    }

    #[test]
    fn a_clip_has_a_name_strip_a_body_and_two_trimming_edges() {
        let (project, _, early, late) = project_with_clips();
        let g = geometry(&project);
        let top = g.lanes.top() + TRACK_HEIGHT;
        let part = |x: f32, y: f32| clip_at(&project.tracks, &g, pos2(x, top + y)).map(|hit| (hit.clip, hit.part));
        assert_eq!(part(g.x(4.0) + 2.0, 30.0), Some((early, ClipPart::Start)));
        assert_eq!(part(g.x(5.0), 8.0), Some((early, ClipPart::Title)));
        assert_eq!(part(g.x(5.0), 30.0), Some((early, ClipPart::Body)));
        assert_eq!(part(g.x(14.0) - 2.0, 30.0), Some((late, ClipPart::End)));
        // An edge trims along the whole height, the name strip included.
        assert_eq!(part(g.x(14.0) - 2.0, 5.0), Some((late, ClipPart::End)));
        // MIDI clips have no fades to grab under the name strip.
        assert_eq!(part(g.x(6.0) + 2.0, CLIP_TITLE + 4.0), Some((late, ClipPart::Start)));
    }

    #[test]
    fn an_audio_clip_has_fade_handles_under_its_name() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let audio = AudioContent::new("take.wav");
        let clip = project.add_audio_clip(track, "Take", 4.0, 8.0, audio).unwrap();
        project.set_clip_fades(clip, 2.0, 1.0).unwrap();
        let g = geometry(&project);
        let band = g.lanes.top() + CLIP_TITLE + 5.0;
        let part = |beat: f64, y: f32| clip_at(&project.tracks, &g, pos2(g.x(beat), y)).map(|hit| hit.part);
        assert_eq!(part(6.0, band), Some(ClipPart::FadeIn), "where the fade in reaches full level");
        assert_eq!(part(11.0, band), Some(ClipPart::FadeOut));
        assert_eq!(part(8.0, band), Some(ClipPart::Body), "between the handles");
        assert_eq!(part(6.0, band + FADE_BAND + 6.0), Some(ClipPart::Body), "below the band the body is grabbed");
        assert_eq!(part(4.05, band + FADE_BAND + 6.0), Some(ClipPart::Start), "and the edge still trims");

        // With no fades the handles wait in the top corners.
        project.set_clip_fades(clip, 0.0, 0.0).unwrap();
        let part = |beat: f64, y: f32| clip_at(&project.tracks, &g, pos2(g.x(beat), y)).map(|hit| hit.part);
        assert_eq!(part(4.1, band), Some(ClipPart::FadeIn));
        assert_eq!(part(11.9, band), Some(ClipPart::FadeOut));
    }

    #[test]
    fn loop_marks_fall_where_the_phrase_starts_over() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let id = project.add_midi_clip(track, "c", 8.0, 10.0).unwrap();
        let note = Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100 };
        project.clip_mut(id).unwrap().add_notes(&[note]).unwrap();
        let marks = |project: &Project| {
            let clip = project.clip(id).unwrap();
            loop_starts(clip, clip.midi().unwrap(), 20.0)
        };
        assert!(marks(&project).is_empty(), "a clip that does not loop has none");

        project.clip_mut(id).unwrap().set_loop_length(Some(4.0)).unwrap();
        assert_eq!(marks(&project), [12.0, 16.0]);
        // Trimming a beat off the front moves the phrase, and the marks
        // with it: they stay on the notes.
        project.trim_clip_start(id, 9.0).unwrap();
        assert_eq!(marks(&project), [12.0, 16.0]);
        let clip = project.clip(id).unwrap();
        let starts: Vec<f64> = clip.timeline_notes().iter().map(|note| note.start).collect();
        assert_eq!(starts, [12.0, 16.0], "where the looped note sounds");

        // A loop as long as the clip, or one too small to see, has no marks.
        project.clip_mut(id).unwrap().set_loop_length(Some(40.0)).unwrap();
        assert!(marks(&project).is_empty());
        project.clip_mut(id).unwrap().set_loop_length(Some(0.1)).unwrap();
        assert!(marks(&project).is_empty());
    }

    #[test]
    fn a_waveform_follows_offset_trimming_and_warping() {
        let mut project = Project::new("t");
        project.set_tempo(120.0).unwrap();
        let track = project.add_track("A");
        let content = AudioContent { offset_seconds: 1.0, ..AudioContent::new("take.wav") };
        let id = project.add_audio_clip(track, "Take", 4.0, 8.0, content).unwrap();
        let second = |project: &Project, beat: f64| {
            let clip = project.clip(id).unwrap();
            file_second(clip, clip.audio().unwrap(), &project.tempo_map(), beat)
        };
        // At 120 a beat is half a second; the clip enters a second in.
        assert_eq!(second(&project, 4.0), 1.0);
        assert_eq!(second(&project, 6.0), 2.0);
        assert_eq!(second(&project, 12.0), 5.0);

        // Trimming the start hides the beginning: what is under each beat
        // of the timeline stays what it was.
        project.trim_clip_start(id, 6.0).unwrap();
        assert_eq!(second(&project, 6.0), 2.0);
        assert_eq!(second(&project, 12.0), 5.0);
        // So does splitting, for the half that is cut off.
        let rest = project.split_clip(id, 8.0).unwrap();
        let half = project.clip(rest).unwrap();
        assert_eq!(file_second(half, half.audio().unwrap(), &TempoMap::constant(120.0), 8.0), 3.0);

        // Under a tempo that changes, a clip at its own speed is as far
        // into its file as the song has been going since it began: the
        // line bends where the tempo does, and nowhere for a clip that
        // follows the tempo.
        let mut song = Project::new("t");
        song.set_tempo_at(8.0, 60.0, false).unwrap();
        let lane = song.add_track("A");
        let take = song.add_audio_clip(lane, "Take", 4.0, 8.0, AudioContent::new("take.wav")).unwrap();
        let (clip, map) = (song.clip(take).unwrap().clone(), song.tempo_map());
        let own = clip.audio().unwrap();
        for (beat, second) in [(4.0, 0.0), (8.0, 2.0), (10.0, 4.0), (12.0, 6.0)] {
            assert_eq!(file_second(&clip, own, &map, beat), second, "at beat {beat}");
        }
        assert_eq!(bends(own, &map, 4.0, 12.0), [8.0]);
        let follows =
            AudioContent { warp: trill_core::project::WarpMode::Beats, source_tempo: Some(120.0), ..own.clone() };
        assert!(bends(&follows, &map, 4.0, 12.0).is_empty());
        assert_eq!(file_second(&clip, &follows, &map, 12.0), 4.0, "eight beats of a recording at 120");

        // Warped, a beat covers a beat of the recording, whatever the
        // song's tempo; unwarped it covers a beat of the song's.
        project.set_clip_warp(id, trill_core::project::WarpMode::Beats, Some(60.0), 0.0).unwrap();
        assert_eq!(second(&project, 7.0), 3.0, "a second per beat at 60");
        project.set_tempo(90.0).unwrap();
        assert_eq!(second(&project, 7.0), 3.0);
        project.set_clip_warp(id, trill_core::project::WarpMode::Off, Some(60.0), 0.0).unwrap();
        assert!((second(&project, 7.0) - (2.0 + 60.0 / 90.0)).abs() < 1e-12);
    }

    #[test]
    fn a_fade_curve_rises_from_silence_to_full_level() {
        assert_eq!(fade_level(0.0), 0.0);
        assert!((fade_level(1.0) - 1.0).abs() < 1e-6);
        // Half the amplitude is 6 dB down, which is most of the way up the
        // level scale: the curve bulges upward.
        assert!(fade_level(0.5) > 0.8 && fade_level(0.5) < 0.9, "{}", fade_level(0.5));
        assert!(fade_level(0.25) < fade_level(0.5));
        assert_eq!(fade_level(-1.0), 0.0);
    }
}
