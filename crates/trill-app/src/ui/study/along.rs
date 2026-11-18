//! What a study found, drawn along a clip of its recording in the
//! arrangement: the parts, the chords and the words of a reference, each
//! where the clip plays it. Where a moment of the recording lies in the
//! song is asked of `trill_runtime::taking`, which knows how the clip is
//! fitted to the song's time.

use super::Open;
use crate::ui::state::Cx;
use eframe::egui::{Align2, Painter, Rect, pos2, vec2};
use trill_core::Clip;
use trill_noir::{TextStyle, fade, paint, text};
use trill_runtime::taking;

/// Height of each of the rows that are drawn along the bottom of a clip.
const ROW: f32 = 11.0;

/// Draws what `open` found along `clip`, whose content is `content` on
/// screen. `x_of` says where a beat of the song is.
pub fn paint(painter: &Painter, cx: &Cx, content: Rect, clip: &Clip, open: &Open, x_of: &dyn Fn(f64) -> f32) {
    let theme = cx.theme;
    let project = &cx.snap.project;
    let small = TextStyle { upper: false, tracking: 0.0, ..theme.text.micro };
    let x = |seconds: f64| taking::beat_of(project, clip, seconds).map(x_of);
    let painter = painter.with_clip_rect(content.intersect(painter.clip_rect()));
    // As many rows as the clip is tall enough for, from the bottom up: parts, chords, words.
    let rows = (((content.height() - 14.0) / ROW).floor() as usize).min(3);
    let row = |index: usize| {
        let bottom = content.bottom() - index as f32 * ROW;
        Rect::from_min_max(pos2(content.left(), bottom - ROW), pos2(content.right(), bottom))
    };
    if rows == 0 {
        return;
    }
    let band = Rect::from_min_max(pos2(content.left(), content.bottom() - rows as f32 * ROW), content.max);
    painter.rect_filled(band, 0.0, fade(theme.bg, 0.72));

    let parts = row(0);
    for part in &open.heard.parts {
        let (Some(from), Some(to)) = (x(part.start), x(part.end)) else { continue };
        let (from, to) = (from.max(content.left()), to.min(content.right()));
        if to - from < 2.0 {
            continue;
        }
        let color = super::view::part_color(theme, &part.label);
        let block = Rect::from_min_max(pos2(from, parts.top() + 1.0), pos2(to - 1.0, parts.bottom() - 1.0));
        painter.rect_filled(block, 0.0, fade(color, 0.45));
        painter.rect_filled(Rect::from_min_size(block.min, vec2(2.0, block.height())), 0.0, color);
        if block.width() > 14.0 {
            let inside = block.shrink2(vec2(5.0, 0.0));
            text::paint(
                &painter.with_clip_rect(block),
                inside,
                Align2::LEFT_CENTER,
                theme.text.micro,
                &part.label,
                theme.ink,
            );
        }
    }
    if rows >= 2
        && let Some(harmony) = &open.heard.harmony
    {
        let chords = row(1);
        let flats = open.heard.flats();
        let mut free = content.left();
        for chord in &harmony.chords {
            let Some(at) = x(chord.start) else { continue };
            if at > content.right() || at + 3.0 < free {
                continue;
            }
            let name = chord.name(flats);
            let wide = text::width(&painter, small, &name);
            paint::vline(&painter, at, chords.y_range(), theme.grid);
            let label = Rect::from_min_size(pos2(at + 3.0, chords.top()), vec2(wide + 2.0, ROW));
            text::paint(&painter, label, Align2::LEFT_CENTER, small, &name, theme.ink_dim);
            free = at + wide + 9.0;
        }
    }
    if rows >= 3
        && let Some(lyrics) = &open.lyrics
    {
        let words = row(2);
        let mut free = content.left();
        for word in lyrics.words() {
            let Some(at) = x(word.start) else { continue };
            if at > content.right() {
                break;
            }
            if at < free {
                continue;
            }
            let said = word.text.trim();
            let wide = text::width(&painter, small, said);
            let label = Rect::from_min_size(pos2(at, words.top()), vec2(wide + 2.0, ROW));
            text::paint(&painter, label, Align2::LEFT_CENTER, small, said, theme.ink_dim);
            free = at + wide + 5.0;
        }
    }
}
