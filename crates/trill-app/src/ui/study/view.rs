//! The workspace: one study laid out.
//!
//! From the top: a line of what it is and how it is heard; the whole
//! recording small, with its parts; then, under the recording's own bars,
//! its parts, chords and words, and its stems as lanes. Beside them, what
//! was found. Everything is placed by hand in the rectangle it is given,
//! so the same view fills the tab along the bottom or a window of its own.

use super::{Open, Selection, Side, Snap, StudyDrag, Whole};
use crate::ui::state::{Cx, DragSource, SoundDrag};
use crate::ui::widgets;
use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, Layout, Mesh, Rect, Response, Role, Sense, Shape, Ui, WidgetInfo,
    pos2, vec2,
};
use std::sync::Arc;
use trill_listen::{Correction, HitKind, MidiPart};
use trill_noir::{Chip, Theme, fade, paint, text};
use trill_runtime::taking::{self, TakeStretch};

const TOOLBAR: f32 = 26.0;
const OVERVIEW: f32 = 22.0;
const RULER: f32 = 16.0;
/// Height of the rows of parts, chords and words.
const ROW: f32 = 15.0;
/// Width of a lane's header: its name, and its mute and solo.
const HEADER: f32 = 124.0;
/// Width of the list beside the lanes; it is left out of a narrow view.
const SIDE: f32 = 292.0;
const SIDE_ROW: f32 = 20.0;
const STATUS: f32 = 18.0;

/// How the lanes map time to the screen.
#[derive(Copy, Clone)]
struct Span {
    left: f32,
    right: f32,
    from: f64,
    per_second: f64,
}

impl Span {
    fn x(&self, seconds: f64) -> f32 {
        self.left + ((seconds - self.from) * self.per_second) as f32
    }

    fn seconds(&self, x: f32) -> f64 {
        self.from + f64::from(x - self.left) / self.per_second
    }

    fn to(&self) -> f64 {
        self.seconds(self.right)
    }
}

/// A chip placed after the last one in a row; returns its response.
fn next_chip(ui: &Ui, theme: &Theme, cursor: &mut f32, row: Rect, id: Id, chip: &Chip) -> Response {
    let width = chip.width(ui, theme);
    let rect =
        Rect::from_min_size(pos2(*cursor, row.center().y - theme.metric.chip / 2.0), vec2(width, theme.metric.chip));
    *cursor += width + theme.metric.gap - 1.0;
    trill_noir::chip(ui, theme, rect, id, chip)
}

/// A caps label placed after the last thing in a row.
fn next_label(ui: &Ui, theme: &Theme, cursor: &mut f32, row: Rect, words: &str, color: Color32) {
    let width = text::width(ui.painter(), theme.text.micro, words).ceil();
    let rect = Rect::from_min_size(pos2(*cursor, row.top()), vec2(width, row.height()));
    text::paint(ui.painter(), rect, Align2::LEFT_CENTER, theme.text.micro, words, color);
    *cursor += width + 6.0;
}

/// The small type, with what it sets left as it is spelled: a chord's name, a word, a path.
fn spelled(theme: &Theme) -> trill_noir::TextStyle {
    trill_noir::TextStyle { upper: false, tracking: 0.0, ..theme.text.micro }
}

fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// The color of a part by its letter: parts that sound alike share one.
fn part_color(theme: &Theme, label: &str) -> Color32 {
    let index = label.bytes().next().map_or(0, |letter| usize::from(letter.saturating_sub(b'A')));
    theme.track_hue(index * 3 + 1)
}

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    ui.expand_to_include_rect(area);
    let painter = ui.painter().clone();
    painter.rect_filled(area, 0.0, theme.bg);
    let Some(open) = cx.studies.open().cloned() else {
        return nothing_open(ui, cx, area);
    };

    let bar = Rect::from_min_size(area.min, vec2(area.width(), TOOLBAR));
    let status = Rect::from_min_max(pos2(area.left(), area.bottom() - STATUS), area.max);
    let body = Rect::from_min_max(pos2(area.left(), bar.bottom()), pos2(area.right(), status.top()));
    let side_width = if body.width() >= 760.0 { SIDE } else { 0.0 };
    let lanes = Rect::from_min_max(body.min, pos2(body.right() - side_width, body.bottom()));
    let side = Rect::from_min_max(pos2(lanes.right(), body.top()), body.max);

    toolbar(ui, cx, &open, bar);
    recording(ui, cx, &open, lanes);
    if side_width > 0.0 {
        beside(ui, cx, &open, side);
    }

    // What the last thing that was asked for said, and where the recording is from.
    painter.rect_filled(status, 0.0, theme.panel);
    paint::hline(&painter, status.x_range(), status.top(), theme.hairline);
    let line = status.shrink2(vec2(10.0, 0.0));
    match cx.studies.last() {
        Some((words, wrong)) => {
            let color = if wrong { theme.signal } else { theme.ink_dim };
            let response = ui.interact(status, ui.scope_id().with("study-said"), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
            text::paint_name(&painter, line, Align2::LEFT_CENTER, theme.text.body, words, color);
        }
        None => {
            text::paint_name(&painter, line, Align2::LEFT_CENTER, spelled(theme), &open.of, theme.ink_faint);
        }
    }
}

/// What the tab shows while no study is open.
fn nothing_open(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let middle = Rect::from_center_size(area.center() - vec2(0.0, 16.0), vec2(area.width() - 40.0, 18.0));
    let words = match (cx.studies.opening(), cx.studies.last(), cx.studies.unavailable()) {
        (Some(_), ..) => "Reading the study\u{2026}".to_owned(),
        (None, Some((words, true)), _) => words.to_owned(),
        (None, _, Some(why)) => why.to_owned(),
        _ if cx.studies.list().is_empty() => {
            "A study takes a recording apart: its stems, its bars, its chords and its words. None has been made yet."
                .to_owned()
        }
        _ => "No study is open. The Studies place of the browser lists them.".to_owned(),
    };
    let response = ui.interact(middle, ui.scope_id().with("study-none"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &words));
    text::paint(&painter, middle, Align2::CENTER_CENTER, theme.text.body, &words, theme.ink_dim);
    let chip = Chip::action("STUDY A RECORDING\u{2026}").named("Study a recording");
    let width = chip.width(ui, theme);
    let rect = Rect::from_center_size(area.center() + vec2(0.0, 12.0), vec2(width, theme.metric.chip));
    let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with("study-ask"), &chip);
    cx.hint(&response, "Take a recording apart: a file, or a link");
    if response.clicked() {
        cx.studies.dialog.open = true;
    }
}

/// The line along the top: what the study is, how it is heard, and what is done with it.
fn toolbar(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, bar: Rect) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);
    let id = ui.scope_id().with("study-bar");
    let mut cursor = bar.left() + 10.0;

    // What it is.
    let title = text::elide_middle(&painter, theme.text.body, &open.title, 220.0);
    let width = text::width(&painter, theme.text.body, &title).ceil();
    let rect = Rect::from_min_size(pos2(cursor, bar.top()), vec2(width, bar.height()));
    let named = ui.interact(rect, id.with("title"), Sense::hover());
    named.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("Study: {}", open.title)));
    text::paint(&painter, rect, Align2::LEFT_CENTER, theme.text.body, &title, theme.ink);
    cursor += width + 10.0;
    let grid = &open.heard.grid;
    let mut facts = vec![format!("{:.1} BPM", grid.bpm)];
    if let Some(key) = open.key {
        facts.push(key.name());
    }
    facts.push(format!("{}/4", grid.beats_per_bar));
    facts.push(clock(open.seconds));
    next_label(ui, theme, &mut cursor, bar, &facts.join("  \u{00B7}  "), theme.ink_dim);
    cursor += 8.0;

    // What a selection's ends are moved onto.
    next_label(ui, theme, &mut cursor, bar, "SNAP", theme.ink_faint);
    for (snap, label, about) in [
        (Snap::Bar, "BAR", "A selection begins and ends on bar lines of the recording"),
        (Snap::Beat, "BEAT", "A selection begins and ends on beats of the recording"),
        (Snap::Off, "OFF", "A selection begins and ends where the pointer is"),
    ] {
        let name = format!("Snap to {}", label.to_lowercase());
        let chip = Chip::new(label, cx.studies.view.snap == snap).named(&name);
        let response = next_chip(ui, theme, &mut cursor, bar, id.with(("snap", label)), &chip);
        cx.hint(&response, about);
        if response.clicked() {
            cx.studies.view.snap = snap;
        }
    }
    cursor += 8.0;

    // How it is heard, which is how it is taken.
    let recorded = cx.studies.view.as_recorded;
    let chip = Chip::new("AS RECORDED", recorded).named("As recorded");
    let response = next_chip(ui, theme, &mut cursor, bar, id.with("recorded"), &chip);
    cx.hint(
        &response,
        "Off: heard and taken in the song\u{2019}s tempo, a beat for a beat. On: at the recording\u{2019}s own speed, untouched",
    );
    if response.clicked() {
        cx.studies.view.as_recorded = !recorded;
        super::stop_sounding(cx);
    }
    let song_key = cx.snap.project.key;
    let moved = match (song_key, open.key) {
        (Some(song), Some(read)) => Some(song.semitones_from(read)),
        _ => None,
    };
    let label = match moved {
        Some(by) if by != 0 => format!("SONG KEY {by:+}"),
        _ => "SONG KEY".to_owned(),
    };
    let on = cx.studies.view.to_song_key && !recorded;
    let chip = Chip::new(&label, on).named("To the song's key").dimmed(moved.is_none() || recorded);
    let response = next_chip(ui, theme, &mut cursor, bar, id.with("key"), &chip);
    cx.hint(
        &response,
        match (song_key, open.key) {
            (None, _) => "The song does not say its key yet: take this recording\u{2019}s under Notes, or say it to the assistant".to_owned(),
            (_, None) => "No key was read in this recording".to_owned(),
            (Some(song), Some(read)) => format!(
                "Moves what is heard and taken from {} to {}, the song\u{2019}s key; drums are left where they are",
                read.name(),
                song.name()
            ),
        },
    );
    if response.clicked() && moved.is_some() && !recorded {
        cx.studies.view.to_song_key = !cx.studies.view.to_song_key;
        super::stop_sounding(cx);
    }
    cursor += 8.0;

    // Heard, and sent.
    let sounding = cx.studies.is_sounding();
    let label = if cx.studies.is_preparing() { "MAKING\u{2026}" } else { "PLAY ROUND" };
    let chip = Chip::new(label, sounding).named("Play round").accent(theme.ok);
    let response = next_chip(ui, theme, &mut cursor, bar, id.with("round"), &chip);
    cx.hint(&response, "Plays the selection round as it would sound in the song, in time with it while it plays");
    if response.clicked() {
        super::toggle_sounding(&ctx, cx);
    }
    let chip = Chip::action("TO THE SONG").named("To the song").dimmed(cx.studies.view.selection.is_none());
    let response = next_chip(ui, theme, &mut cursor, bar, id.with("send"), &chip);
    cx.hint(
        &response,
        "Puts the selection on the selected track at the edit cursor, or on a track of its own \u{00B7} Enter",
    );
    if response.clicked() {
        super::to_the_song(&ctx, cx);
    }
    cursor += 8.0;

    // The grid, read another way.
    if bar.right() - cursor > 330.0 {
        next_label(ui, theme, &mut cursor, bar, "GRID", theme.ink_faint);
        let has = |wanted: Correction| open.corrections.contains(&wanted);
        let without = |kinds: &[Correction]| -> Vec<Correction> {
            open.corrections.iter().copied().filter(|correction| !kinds.contains(correction)).collect()
        };
        for (correction, label, name, about) in [
            (Correction::Half, "\u{00BD}", "Half the tempo", "Every other beat is a beat: half the tempo"),
            (
                Correction::Double,
                "\u{00D7}2",
                "Double the tempo",
                "There is a beat between every two: double the tempo",
            ),
        ] {
            let chip = Chip::new(label, has(correction)).named(name);
            let response = next_chip(ui, theme, &mut cursor, bar, id.with(("grid", name)), &chip);
            cx.hint(&response, about);
            if response.clicked() {
                let mut wanted = without(&[Correction::Half, Correction::Double]);
                if !has(correction) {
                    wanted.push(correction);
                }
                super::correct(&ctx, cx, wanted);
            }
        }
        let chip = Chip::action("+1").named("The bar a beat later");
        let response = next_chip(ui, theme, &mut cursor, bar, id.with("rotate"), &chip);
        cx.hint(&response, "The bar starts one beat later");
        if response.clicked() {
            let mut wanted = open.corrections.clone();
            wanted.push(Correction::Rotate);
            super::correct(&ctx, cx, wanted);
        }
        for beats in [3u8, 4, 6] {
            let label = beats.to_string();
            let name = format!("{beats} beats to a bar");
            let chip = Chip::new(&label, grid.beats_per_bar == usize::from(beats)).named(&name);
            let response = next_chip(ui, theme, &mut cursor, bar, id.with(("per-bar", beats)), &chip);
            cx.hint(&response, format!("Counts {beats} beats to a bar"));
            if response.clicked() && grid.beats_per_bar != usize::from(beats) {
                let mut wanted: Vec<Correction> = open
                    .corrections
                    .iter()
                    .copied()
                    .filter(|correction| !matches!(correction, Correction::BeatsPerBar(_)))
                    .collect();
                wanted.push(Correction::BeatsPerBar(beats));
                super::correct(&ctx, cx, wanted);
            }
        }
        if !open.corrections.is_empty() {
            let chip = Chip::action("AS FOUND").named("The grid as it was found");
            let response = next_chip(ui, theme, &mut cursor, bar, id.with("found"), &chip);
            cx.hint(&response, "Takes every correction off: the grid as the study found it");
            if response.clicked() {
                super::correct(&ctx, cx, Vec::new());
            }
        }
    }

    // A window of its own, at the far end.
    let chip = Chip::new("WINDOW", cx.studies.apart).named("In a window of its own");
    let width = chip.width(ui, theme);
    if bar.right() - cursor > width + 16.0 {
        let rect = Rect::from_min_size(
            pos2(bar.right() - width - 10.0, bar.center().y - theme.metric.chip / 2.0),
            vec2(width, theme.metric.chip),
        );
        let response = trill_noir::chip(ui, theme, rect, id.with("apart"), &chip);
        cx.hint(&response, "Shows the study in a window of its own, to put beside the song or on another screen");
        if response.clicked() {
            cx.studies.apart = !cx.studies.apart;
        }
    }
}

/// The recording: its overview, its bars, its parts, chords and words, and its stems as lanes.
fn recording(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, area: Rect) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    let id = ui.scope_id().with("study-lanes");
    let seconds = open.seconds.max(0.1);
    let wave_left = area.left() + HEADER;
    let width = f64::from((area.right() - wave_left).max(40.0));

    // The view: all of it unless it was zoomed, and never past its ends.
    let fit = width / seconds;
    let view = &mut cx.studies.view;
    if view.per_second <= 0.0 || view.per_second < fit {
        (view.per_second, view.from) = (fit, 0.0);
    }
    view.per_second = view.per_second.min(600.0);
    view.from = view.from.clamp(0.0, (seconds - width / view.per_second).max(0.0));
    let span = Span { left: wave_left, right: area.right(), from: view.from, per_second: view.per_second };

    let overview = Rect::from_min_size(area.min, vec2(area.width(), OVERVIEW));
    let ruler = Rect::from_min_size(pos2(area.left(), overview.bottom()), vec2(area.width(), RULER));
    let has_words = open.lyrics.as_ref().is_some_and(|lyrics| !lyrics.lines.is_empty());
    let rows = [true, open.heard.harmony.is_some(), has_words].into_iter().filter(|shown| *shown).count();
    let rows_rect = Rect::from_min_size(pos2(area.left(), ruler.bottom()), vec2(area.width(), rows as f32 * ROW));
    let stems = Rect::from_min_max(pos2(area.left(), rows_rect.bottom()), area.max);
    let lane_height = (stems.height() / open.stems.len().max(1) as f32).max(18.0);

    whole(ui, cx, open, overview, span);

    // The bars of the recording.
    painter.rect_filled(ruler, 0.0, theme.panel);
    paint::hline(&painter, ruler.x_range(), ruler.top(), theme.hairline);
    paint::hline_above(&painter, ruler.x_range(), ruler.bottom(), theme.hairline);
    text::paint(
        &painter,
        ruler.shrink2(vec2(10.0, 0.0)),
        Align2::LEFT_CENTER,
        theme.text.micro,
        "BARS",
        theme.ink_faint,
    );
    let bar_width = open.bars.windows(2).next().map_or(60.0, |pair| ((pair[1] - pair[0]) * span.per_second) as f32);
    let every = [1usize, 2, 4, 8, 16, 32].into_iter().find(|every| bar_width * *every as f32 >= 34.0).unwrap_or(64);
    let waves = Rect::from_min_max(pos2(wave_left, ruler.top()), area.max);
    let lined = painter.with_clip_rect(waves.intersect(ui.clip_rect()));
    for (index, line) in open.bars.iter().enumerate() {
        let x = span.x(*line);
        if x < wave_left - 1.0 || x > area.right() + 1.0 {
            continue;
        }
        let numbered = index % every == 0 && index + 1 < open.bars.len();
        paint::vline(&lined, x, ruler.bottom() - if numbered { 9.0 } else { 4.0 }..=ruler.bottom(), theme.ink_faint);
        if numbered {
            let label = Rect::from_min_size(pos2(x + 3.0, ruler.top()), vec2(40.0, RULER));
            text::paint(&lined, label, Align2::LEFT_CENTER, theme.text.micro, &(index + 1).to_string(), theme.ink_dim);
        }
        paint::vline(&lined, x, rows_rect.top()..=area.bottom(), theme.grid);
    }
    // Beats, where there is room between them.
    if bar_width / open.heard.grid.beats_per_bar.max(1) as f32 >= 14.0 {
        for beat in &open.heard.grid.beats {
            let x = span.x(*beat);
            if x >= wave_left && x <= area.right() {
                paint::vline(&lined, x, stems.top()..=area.bottom(), theme.grid_fine);
            }
        }
    }

    // Parts, chords and words along the bars.
    let mut top = rows_rect.top();
    let mut row = |label: &str| {
        let rect = Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), ROW));
        top += ROW;
        painter.rect_filled(Rect::from_min_max(rect.min, pos2(wave_left, rect.bottom())), 0.0, theme.panel);
        text::paint(
            &painter,
            rect.shrink2(vec2(10.0, 0.0)),
            Align2::LEFT_CENTER,
            theme.text.micro,
            label,
            theme.ink_faint,
        );
        paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);
        rect
    };
    let parts_row = row("PARTS");
    for part in &open.heard.parts {
        let (from, to) = (span.x(part.start).max(wave_left), span.x(part.end).min(area.right()));
        if to - from < 2.0 {
            continue;
        }
        let color = part_color(theme, &part.label);
        let block = Rect::from_min_max(pos2(from, parts_row.top() + 2.0), pos2(to - 1.0, parts_row.bottom() - 2.0));
        lined.rect_filled(block, 0.0, fade(color, 0.28));
        lined.rect_filled(Rect::from_min_size(block.min, vec2(2.0, block.height())), 0.0, color);
        let without = match part.without.as_slice() {
            [] => String::new(),
            without => format!("  no {}", without.join(", ")),
        };
        let words = format!("{}{without}", part.label);
        let inside = block.shrink2(vec2(6.0, 0.0));
        if inside.width() > 10.0 {
            text::paint_name(
                &lined.with_clip_rect(block.intersect(lined.clip_rect())),
                inside,
                Align2::LEFT_CENTER,
                theme.text.micro,
                &words,
                theme.ink,
            );
        }
    }
    if let Some(harmony) = &open.heard.harmony {
        let chords_row = row("CHORDS");
        let flats = open.heard.flats();
        let mut free = wave_left;
        for chord in &harmony.chords {
            let x = span.x(chord.start);
            if x > area.right() || span.x(chord.end) < wave_left {
                continue;
            }
            paint::vline(&lined, x, chords_row.y_range(), theme.grid);
            let name = chord.name(flats);
            let wide = text::width(&lined, spelled(theme), &name);
            if x + 3.0 >= free {
                let label = Rect::from_min_size(pos2(x + 3.0, chords_row.top()), vec2(wide + 2.0, ROW));
                text::paint(&lined, label, Align2::LEFT_CENTER, spelled(theme), &name, theme.ink_dim);
                free = x + wide + 9.0;
            }
        }
    }
    if has_words {
        let words_row = row("WORDS");
        words(ui, cx, open, words_row, span, &lined);
    }

    // The stems.
    let hair = paint::hair(&painter);
    let mut mesh = Mesh::default();
    let columns = trill_noir::waveform_columns(&painter, area.right() - wave_left);
    for (index, (name, file, there)) in open.stems.iter().enumerate() {
        let lane = Rect::from_min_size(
            pos2(area.left(), stems.top() + index as f32 * lane_height),
            vec2(area.width(), lane_height),
        );
        let header = Rect::from_min_max(lane.min, pos2(wave_left, lane.bottom()));
        let wave = Rect::from_min_max(pos2(wave_left, lane.top()), lane.max);
        painter.rect_filled(header, 0.0, theme.panel);
        paint::hline_above(&painter, lane.x_range(), lane.bottom(), theme.hairline);
        paint::vline_left_of(&painter, wave_left, lane.y_range(), theme.hairline);
        let color = theme.track_hue(index * 2);
        painter.rect_filled(Rect::from_min_size(header.min, vec2(3.0, header.height() - 1.0)), 0.0, color);
        let label = Rect::from_min_max(header.min + vec2(10.0, 0.0), pos2(header.right() - 44.0, header.bottom()));
        text::paint_name(
            &painter,
            label,
            Align2::LEFT_CENTER,
            theme.text.caps,
            name,
            if *there { theme.ink } else { theme.ink_faint },
        );

        // Mute and solo: what is heard is what would be taken.
        let muted = cx.studies.view.muted.contains(name);
        let soloed = cx.studies.view.solo.contains(name);
        let chip_top = header.center().y - theme.metric.chip / 2.0;
        let mute_name = format!("Mute {name}");
        let mute = Rect::from_min_size(pos2(header.right() - 42.0, chip_top), vec2(18.0, theme.metric.chip));
        let response = trill_noir::chip(
            ui,
            theme,
            mute,
            id.with(("mute", index)),
            &Chip::new("M", muted).named(&mute_name).accent(theme.signal),
        );
        cx.hint(&response, "Leaves this stem out of what is heard and taken");
        if response.clicked() {
            if !cx.studies.view.muted.remove(name) {
                cx.studies.view.muted.insert(name.clone());
            }
            super::stop_sounding(cx);
        }
        let solo_name = format!("Solo {name}");
        let solo = Rect::from_min_size(pos2(header.right() - 22.0, chip_top), vec2(18.0, theme.metric.chip));
        let response = trill_noir::chip(
            ui,
            theme,
            solo,
            id.with(("solo", index)),
            &Chip::new("S", soloed).named(&solo_name).accent(theme.ok),
        );
        cx.hint(&response, "Hears and takes this stem alone, with any other that is soloed");
        if response.clicked() {
            if !cx.studies.view.solo.remove(name) {
                cx.studies.view.solo.insert(name.clone());
            }
            super::stop_sounding(cx);
        }

        // Its outline: from the file once a worker has read it, from what the study kept until then.
        let heard = if cx.studies.view.solo.is_empty() { !muted } else { soloed };
        let ink = if heard && *there { color } else { fade(theme.ink_faint, 0.5) };
        let from_file = there.then(|| cx.sounds.wave(&file.to_string_lossy())).and_then(|state| state.ready().cloned());
        let bands: Vec<(f32, f32)> = match (&from_file, &open.outlines) {
            (Some(wave), _) => wave
                .columns(span.from..span.to(), columns)
                .iter()
                .map(|column| (column.mixed.min, column.mixed.max))
                .collect(),
            (None, Some(outlines)) => {
                let outline = outlines.stems.iter().find(|outline| &outline.name == name);
                let per_column = (span.to() - span.from) / columns.max(1) as f64 * outlines.per_second;
                (0..columns)
                    .map(|column| {
                        let Some(outline) = outline else { return (0.0, 0.0) };
                        let first = ((span.from * outlines.per_second) + column as f64 * per_column) as usize;
                        let last = (first + per_column.ceil().max(1.0) as usize).min(outline.min.len());
                        let low = outline.min.get(first..last).unwrap_or_default().iter().copied().min().unwrap_or(0);
                        let high = outline.max.get(first..last).unwrap_or_default().iter().copied().max().unwrap_or(0);
                        (f32::from(low) / 127.0, f32::from(high) / 127.0)
                    })
                    .collect()
            }
            (None, None) => Vec::new(),
        };
        trill_noir::waveform(&mut mesh, wave.shrink2(vec2(0.0, 2.0)), &bands, 1.0, ink, hair);
        if !there {
            let gone = wave.shrink2(vec2(8.0, 0.0));
            text::paint(
                &lined,
                gone,
                Align2::LEFT_CENTER,
                theme.text.micro,
                "THE STEMS WERE DROPPED: WHAT WAS FOUND IS KEPT",
                theme.ink_faint,
            );
        }
    }
    lined.add(Shape::mesh(Arc::new(mesh)));

    // The selection, over the lanes it covers.
    if let Some(selection) = cx.studies.view.selection.clone() {
        let rect = Rect::from_min_max(
            pos2(span.x(selection.from), stems.top() + selection.lanes.0 as f32 * lane_height),
            pos2(span.x(selection.to), stems.top() + (selection.lanes.1 + 1) as f32 * lane_height),
        );
        lined.rect_filled(rect, 0.0, theme.selected);
        paint::frame(&lined, rect, theme.ink_dim);
        // And in the ruler, so that its bars can be read off.
        let mark = Rect::from_min_max(pos2(rect.left(), ruler.bottom() - 3.0), pos2(rect.right(), ruler.bottom()));
        lined.rect_filled(mark, 0.0, theme.ink_dim);
    }

    // Selecting, and carrying a selection off.
    let waves = Rect::from_min_max(pos2(wave_left, stems.top()), area.max);
    let response = ui.interact(waves, id.with("select"), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Stems of the study"));
    let lane_at =
        |y: f32| (((y - stems.top()) / lane_height).floor().max(0.0) as usize).min(open.stems.len().saturating_sub(1));
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let inside = |selection: &Selection, at: egui::Pos2| {
        let seconds = span.seconds(at.x);
        let lane = lane_at(at.y);
        seconds >= selection.from && seconds <= selection.to && (selection.lanes.0..=selection.lanes.1).contains(&lane)
    };
    let over = pointer.zip(cx.studies.view.selection.clone()).filter(|(at, selection)| inside(selection, *at));
    if over.is_some() && !response.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
        cx.state.hint.set(
            "Drag the selection onto a track of the song, or below the tracks for a new one \u{00B7} Enter sends it",
        );
    } else if response.hovered() {
        cx.state
            .hint
            .set("Drag across the lanes to select a stretch \u{00B7} double-click for a bar \u{00B7} Ctrl+wheel zooms");
    }
    // A drag is what it was where the button went down, not where the pointer has got to since.
    let pressed = ui.input(|input| input.pointer.press_origin());
    if response.drag_started()
        && let Some(at) = pressed.or(pointer)
    {
        let picked_up = cx.studies.view.selection.clone().filter(|selection| inside(selection, at));
        match picked_up {
            Some(_) => {
                if let Some(take) = super::selected(cx) {
                    let beats = super::beats_of(cx, open, &take);
                    begin_drag(cx, at, StudyDrag::Stretch(take.clone(), beats), stretch_label(open, &take));
                }
            }
            None => {
                let seconds = open.snapped(span.seconds(at.x), cx.studies.view.snap);
                cx.studies.view.anchor = Some((seconds, lane_at(at.y)));
                super::stop_sounding(cx);
            }
        }
    }
    if response.dragged()
        && let (Some(at), Some((anchor, lane))) = (pointer, cx.studies.view.anchor)
    {
        let seconds = open.snapped(span.seconds(at.x.clamp(wave_left, area.right())), cx.studies.view.snap);
        let (from, to) = (anchor.min(seconds), anchor.max(seconds));
        let here = lane_at(at.y);
        cx.studies.view.selection =
            (to - from > 0.01).then_some(Selection { from, to, lanes: (lane.min(here), lane.max(here)) });
    }
    if response.drag_stopped() {
        cx.studies.view.anchor = None;
    }
    if widgets::double_clicked(&response)
        && let Some(at) = pointer
    {
        // The bar under the pointer, on every lane.
        let seconds = span.seconds(at.x);
        let after = open.bars.partition_point(|line| *line <= seconds);
        if let (Some(from), Some(to)) =
            (after.checked_sub(1).and_then(|index| open.bars.get(index)), open.bars.get(after))
        {
            cx.studies.view.selection =
                Some(Selection { from: *from, to: *to, lanes: (0, open.stems.len().saturating_sub(1)) });
        }
    } else if response.clicked() && over.is_none() {
        cx.studies.view.selection = None;
        super::stop_sounding(cx);
    }

    // The wheel: along the recording, and with Ctrl into it.
    if response.hovered() || ui.rect_contains_pointer(Rect::from_min_max(pos2(wave_left, ruler.top()), area.max)) {
        let (scroll, zoom, at) =
            ui.input(|input| (input.smooth_scroll_delta, input.zoom_delta(), input.pointer.hover_pos()));
        let view = &mut cx.studies.view;
        if (zoom - 1.0).abs() > 1e-3 {
            let anchor = at.map_or(span.from, |at| span.seconds(at.x));
            let per_second = (view.per_second * f64::from(zoom)).clamp(fit, 600.0);
            view.from = anchor - (anchor - view.from) * view.per_second / per_second;
            view.per_second = per_second;
        }
        let along = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
        if along.abs() > 0.0 {
            view.from -= f64::from(along) / view.per_second;
        }
    }
    // Enter sends the selection to the song while the pointer is over the study.
    let entered = ui.rect_contains_pointer(area) && ui.input(|input| input.key_pressed(egui::Key::Enter));
    if entered && cx.studies.view.word.is_none() && !ui.ctx().text_edit_focused() {
        super::to_the_song(&ctx, cx);
    }
}

/// What a stretch is called while it is in the air.
fn stretch_label(open: &Open, take: &TakeStretch) -> String {
    let stems = match take.stems.len() {
        count if count == open.stems.len() => "Recording".to_owned(),
        _ => take.stems.join(" + "),
    };
    let (first, last) = (taking::bar_of(&open.heard, take.from + 1e-3), taking::bar_of(&open.heard, take.to - 1e-3));
    if first == last { format!("{stems}, bar {first}") } else { format!("{stems}, bars {first}\u{2013}{last}") }
}

/// Picks something of the study up, to be carried to the song.
fn begin_drag(cx: &mut Cx, at: egui::Pos2, drag: StudyDrag, label: String) {
    cx.state.sound_drag = Some(SoundDrag {
        source: DragSource::Study,
        paths: Vec::new(),
        label,
        seconds: None,
        tempo: None,
        tags: trill_library::Tags::EMPTY,
        pointer: Some(at),
        target: None,
        refusal: None,
        study: Some(drag),
    });
}

/// The whole recording, small: its parts, where the view is, what is selected.
fn whole(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, rect: Rect, span: Span) {
    let theme = cx.theme;
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, 0.0, theme.panel);
    let strip = Rect::from_min_max(pos2(span.left, rect.top() + 3.0), pos2(span.right, rect.bottom() - 3.0));
    let label = Rect::from_min_max(rect.min + vec2(10.0, 0.0), pos2(span.left, rect.bottom()));
    text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.micro, "RECORDING", theme.ink_faint);
    let seconds = open.seconds.max(0.1);
    let x = |at: f64| strip.left() + (at / seconds) as f32 * strip.width();
    painter.rect_filled(strip, 0.0, theme.bg);
    for part in &open.heard.parts {
        let block = Rect::from_min_max(pos2(x(part.start), strip.top()), pos2(x(part.end) - 1.0, strip.bottom()));
        painter.rect_filled(block, 0.0, fade(part_color(theme, &part.label), 0.22));
    }
    // The whole of it as one outline: the loudest of its stems in each column.
    if let Some(outlines) = &open.outlines {
        let columns = trill_noir::waveform_columns(&painter, strip.width());
        let longest = outlines.stems.iter().map(|outline| outline.max.len()).max().unwrap_or(0);
        let per_column = longest as f64 / columns.max(1) as f64;
        let bands: Vec<(f32, f32)> = (0..columns)
            .map(|column| {
                let first = (column as f64 * per_column) as usize;
                let last = (first + per_column.ceil().max(1.0) as usize).min(longest);
                let mut high = 0i8;
                for outline in &outlines.stems {
                    high = high.max(
                        outline
                            .max
                            .get(first..last.min(outline.max.len()))
                            .unwrap_or_default()
                            .iter()
                            .copied()
                            .max()
                            .unwrap_or(0),
                    );
                }
                let level = f32::from(high) / 127.0;
                (-level, level)
            })
            .collect();
        let mut mesh = Mesh::default();
        trill_noir::waveform(&mut mesh, strip, &bands, 1.0, fade(theme.ink_dim, 0.7), paint::hair(&painter));
        painter.add(Shape::mesh(Arc::new(mesh)));
    }
    if let Some(selection) = &cx.studies.view.selection {
        let chosen = Rect::from_min_max(
            pos2(x(selection.from), strip.top()),
            pos2(x(selection.to).max(x(selection.from) + 1.0), strip.bottom()),
        );
        painter.rect_filled(chosen, 0.0, theme.selected);
    }
    // Where the lanes are looking, when they do not show it all.
    let window = Rect::from_min_max(pos2(x(span.from), strip.top()), pos2(x(span.to().min(seconds)), strip.bottom()));
    if window.width() < strip.width() - 2.0 {
        paint::frame(&painter, window, theme.ink_dim);
    }
    paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);
    // A press or a drag in it moves the view there.
    let response = ui.interact(strip, ui.scope_id().with("study-whole"), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "The whole recording"));
    cx.hint(&response, "The whole recording: click or drag to look at another part of it");
    if let Some(at) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
        let middle = f64::from((at.x - strip.left()) / strip.width()) * seconds;
        let shown = span.to() - span.from;
        cx.studies.view.from = (middle - shown / 2.0).clamp(0.0, (seconds - shown).max(0.0));
    }
}

/// The words along the bars: each where it is sung. A double-click puts one right.
fn words(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, row: Rect, span: Span, painter: &egui::Painter) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let Some(lyrics) = &open.lyrics else { return };
    let id = ui.scope_id().with("study-words");
    let mut free = span.left;
    for (line_index, line) in lyrics.lines.iter().enumerate() {
        if span.x(line.end) < span.left || span.x(line.start) > span.right {
            continue;
        }
        // Each word stands where it is sung, where there is room for it: far out the first of a few, close in all.
        let small = spelled(theme);
        for (word_index, word) in line.words.iter().enumerate() {
            let x = span.x(word.start);
            let wide = text::width(painter, small, word.text.trim()) + 4.0;
            let rect = Rect::from_min_size(pos2(x, row.top()), vec2(wide.max(12.0), row.height()));
            if rect.right() < span.left || rect.left() > span.right || rect.left() < free - 0.5 {
                continue;
            }
            free = rect.right() + 3.0;
            let editing =
                cx.studies.view.word.as_ref().is_some_and(|(line, word, _)| (*line, *word) == (line_index, word_index));
            if editing {
                continue;
            }
            let response = ui.interact(rect, id.with((line_index, word_index)), Sense::click());
            let said = word.text.trim().to_owned();
            response.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("Word: {said}")));
            // A word the transcriber was unsure of is fainter: the first place to look for one that is wrong.
            let ink = if response.hovered() {
                theme.ink
            } else if word.p < 0.5 {
                theme.ink_faint
            } else {
                theme.ink_dim
            };
            text::paint(painter, rect, Align2::LEFT_CENTER, small, word.text.trim(), ink);
            cx.hint(&response, "A word where it is sung \u{00B7} double-click to put it right; its time stays");
            if widgets::double_clicked(&response) {
                cx.studies.view.word = Some((line_index, word_index, word.text.trim().to_owned()));
            }
        }
    }
    // The word that is being put right.
    if let Some((line, word, mut typed)) = cx.studies.view.word.clone() {
        let Some(found) = lyrics.lines.get(line).and_then(|line| line.words.get(word)) else {
            cx.studies.view.word = None;
            return;
        };
        let x = span.x(found.start).clamp(span.left, span.right - 120.0);
        let rect = Rect::from_min_size(pos2(x, row.top() - 2.0), vec2(120.0, row.height() + 4.0));
        let field = id.with("typing");
        let response = widgets::place(ui, rect, "word", Layout::left_to_right(Align::Center), |ui| {
            trill_noir::text_field(ui, theme, rect, field, &mut typed, "the word")
        });
        response.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, "The word as it is sung"));
        if !response.has_focus() && !response.lost_focus() {
            response.request_focus();
        }
        let (entered, escaped) =
            ui.input(|input| (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape)));
        cx.studies.view.word = Some((line, word, typed));
        if escaped {
            cx.studies.view.word = None;
        } else if entered || response.lost_focus() {
            super::correct_word(&ctx, cx);
        }
    }
}

/// What was found, beside the lanes.
fn beside(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, area: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    painter.rect_filled(area, 0.0, theme.panel);
    paint::vline(&painter, area.left(), area.y_range(), theme.hairline);
    let id = ui.scope_id().with("study-side");
    let heard = &open.heard;
    let lines = open.lyrics.as_ref().map_or(0, |lyrics| lyrics.lines.len());
    let titles = [
        (Side::Loops, format!("LOOPS {}", heard.loops.len())),
        (Side::Hits, format!("HITS {}", heard.one_shots.len())),
        (Side::Words, format!("LINES {lines}")),
        (Side::Notes, "NOTES".to_owned()),
    ];
    let tabs = Rect::from_min_size(area.min + vec2(8.0, 0.0), vec2(area.width() - 16.0, OVERVIEW));
    let mut cursor = tabs.left();
    for (side, title) in &titles {
        let name = title.split(' ').next().unwrap_or(title);
        let chip = Chip::new(title, cx.studies.view.side == *side).named(name);
        if next_chip(ui, theme, &mut cursor, tabs, id.with(("tab", name)), &chip).clicked() {
            (cx.studies.view.side, cx.studies.view.side_scroll) = (*side, 0.0);
        }
    }
    paint::hline_above(&painter, area.x_range(), tabs.bottom(), theme.hairline);
    let body = Rect::from_min_max(pos2(area.left() + 1.0, tabs.bottom()), area.max);
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("study-side-body").max_rect(body));
    child.set_clip_rect(body.intersect(ui.clip_rect()));
    match cx.studies.view.side {
        Side::Loops => loops(&mut child, cx, open, body),
        Side::Hits => hits(&mut child, cx, open, body),
        Side::Words => lines_of(&mut child, cx, open, body),
        Side::Notes => notes(&mut child, cx, open, body),
    }
}

/// One row of a list: what it says, what stands at its right, whether it is the one chosen.
fn side_row(ui: &mut Ui, cx: &mut Cx, rect: Rect, id: Id, words: &str, right: &str, chosen: bool) -> Response {
    let theme = cx.theme;
    let response = ui.interact(rect, id, Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, chosen, words));
    let painter = ui.painter();
    if chosen {
        painter.rect_filled(rect, 0.0, theme.raised);
    } else if response.hovered() {
        painter.rect_filled(rect, 0.0, theme.hover);
    }
    let inside = rect.shrink2(vec2(10.0, 0.0));
    let taken = text::paint(painter, inside, Align2::RIGHT_CENTER, theme.text.micro, right, theme.ink_faint);
    let room = Rect::from_min_max(inside.min, pos2(inside.right() - taken - 8.0, inside.bottom()));
    text::paint_name(
        painter,
        room,
        Align2::LEFT_CENTER,
        theme.text.body,
        words,
        if chosen { theme.ink } else { theme.ink_dim },
    );
    response
}

/// The rows of a list that are on screen, scrolled by the wheel: the rectangle of each with its number.
fn scrolled(ui: &Ui, cx: &mut Cx, body: Rect, count: usize, height: f32) -> Vec<(usize, Rect)> {
    let total = count as f32 * height;
    if ui.rect_contains_pointer(body) {
        let delta = ui.input(|input| input.smooth_scroll_delta.y);
        cx.studies.view.side_scroll -= delta;
    }
    cx.studies.view.side_scroll = cx.studies.view.side_scroll.clamp(0.0, (total - body.height()).max(0.0));
    let scroll = cx.studies.view.side_scroll;
    (0..count)
        .map(|index| {
            (
                index,
                Rect::from_min_size(
                    pos2(body.left(), body.top() + index as f32 * height - scroll),
                    vec2(body.width(), height),
                ),
            )
        })
        .filter(|(_, rect)| rect.bottom() > body.top() && rect.top() < body.bottom())
        .collect()
}

fn loops(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, body: Rect) {
    let ctx = ui.ctx().clone();
    let id = ui.scope_id().with("loops");
    if open.heard.loops.is_empty() {
        return empty(ui, cx, body, "No stretch of this recording was found worth looping.");
    }
    for (index, rect) in scrolled(ui, cx, body, open.heard.loops.len(), SIDE_ROW) {
        let found = &open.heard.loops[index];
        let bars = if found.bars == 1 { "1 bar".to_owned() } else { format!("{} bars", found.bars) };
        let words = format!("{}  \u{00B7}  {bars} from bar {}", found.label, found.bar + 1);
        let alone = if found.solo { "ALONE  " } else { "" };
        let right = format!("{alone}{:.0}", found.score * 100.0);
        let lanes: Vec<usize> = open
            .stems
            .iter()
            .enumerate()
            .filter(|(_, (name, _, _))| found.stems.contains(name))
            .map(|(index, _)| index)
            .collect();
        let stretch = Selection {
            from: found.start,
            to: found.end,
            lanes: (
                lanes.first().copied().unwrap_or(0),
                lanes.last().copied().unwrap_or(open.stems.len().saturating_sub(1)),
            ),
        };
        let chosen = cx.studies.view.selection.as_ref() == Some(&stretch);
        let hint = "A stretch worth looping, best first \u{00B7} click selects it, double-click plays it round, drag it to the song";
        let response = side_row(ui, cx, rect, id.with(index), &words, &right, chosen);
        cx.hint(&response, hint);
        if response.clicked() || response.drag_started() {
            cx.studies.view.selection = Some(stretch);
            super::stop_sounding(cx);
            // Brought into view if it is not.
            let shown = f64::from(body.left() - HEADER) / cx.studies.view.per_second.max(1e-6);
            if found.start < cx.studies.view.from || found.end > cx.studies.view.from + shown {
                cx.studies.view.from = (found.start - 1.0).max(0.0);
            }
        }
        if widgets::double_clicked(&response) {
            super::toggle_sounding(&ctx, cx);
        }
        if response.drag_started()
            && let (Some(at), Some(mut take)) = (response.interact_pointer_pos(), super::selected(cx))
        {
            // A loop is its own stems, whatever is muted beside it.
            take.stems.clone_from(&found.stems);
            take.name = Some(format!("{} loop, {bars} from bar {} ({})", found.label, found.bar + 1, open.title));
            let beats = super::beats_of(cx, open, &take);
            begin_drag(cx, at, StudyDrag::Stretch(take, beats), format!("{} loop, {bars}", found.label));
        }
    }
}

fn hits(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, body: Rect) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let id = ui.scope_id().with("hits");
    if open.heard.one_shots.is_empty() {
        return empty(ui, cx, body, "No single hit stands clear enough of the rest in this recording.");
    }
    // Pads, three to a row: the drums first, then the bass by pitch.
    const PER_ROW: usize = 3;
    const PAD: f32 = 34.0;
    let count = open.heard.one_shots.len();
    let width = (body.width() - 16.0) / PER_ROW as f32;
    for (row, rect) in scrolled(ui, cx, body, count.div_ceil(PER_ROW), PAD) {
        for column in 0..PER_ROW {
            let Some(hit) = open.heard.one_shots.get(row * PER_ROW + column) else { break };
            let pad = Rect::from_min_size(
                pos2(rect.left() + 8.0 + column as f32 * width, rect.top() + 3.0),
                vec2(width - 4.0, PAD - 6.0),
            );
            let response = ui.interact(pad, id.with(&hit.id), Sense::click_and_drag());
            response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &hit.label));
            let painter = ui.painter();
            painter.rect_filled(pad, 0.0, if response.hovered() { theme.hover } else { theme.bg });
            paint::frame(painter, pad, theme.hairline);
            let color = match hit.kind {
                HitKind::Bass => theme.track_hue(4),
                _ => theme.track_hue(2),
            };
            painter.rect_filled(Rect::from_min_size(pad.min, vec2(pad.width(), 2.0)), 0.0, color);
            text::paint_name(
                painter,
                pad.shrink2(vec2(6.0, 0.0)),
                Align2::LEFT_CENTER,
                theme.text.body,
                &hit.label,
                theme.ink_dim,
            );
            cx.hint(&response, "A single hit \u{00B7} click plays it, drag it onto a track, a pad or a sampler");
            if response.clicked() {
                super::sound_hit(&ctx, cx, hit.id.clone());
            }
            if response.drag_started()
                && let Some(at) = response.interact_pointer_pos()
            {
                begin_drag(cx, at, StudyDrag::Hit { study: open.id.clone(), id: hit.id.clone() }, hit.label.clone());
                if let Some(drag) = &mut cx.state.sound_drag {
                    drag.seconds = Some(hit.end - hit.start);
                }
            }
        }
    }
}

fn lines_of(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, body: Rect) {
    let id = ui.scope_id().with("lines");
    let Some(lyrics) = open.lyrics.as_ref().filter(|lyrics| !lyrics.lines.is_empty()) else {
        return empty(ui, cx, body, "This study wrote down no words: none were asked for, or nobody sings.");
    };
    let vocals = open.stems.iter().position(|(name, _, _)| name == "vocals").unwrap_or(0);
    for (index, rect) in scrolled(ui, cx, body, lyrics.lines.len(), SIDE_ROW) {
        let line = &lyrics.lines[index];
        // From bar line to bar line, so that it keeps its place in the bar.
        let (from, to) = taking::whole_bars(&open.heard, line.start, line.end);
        let stretch = Selection { from, to, lanes: (vocals, vocals) };
        let chosen = cx.studies.view.selection.as_ref() == Some(&stretch);
        let right = open.bar_at(line.start).map_or_else(|| clock(line.start), |bar| format!("BAR {bar}"));
        let hint = "A sung line \u{00B7} click selects it in the vocal, drag it to the song as that stretch, named by its words";
        let response = side_row(ui, cx, rect, id.with(index), line.text.trim(), &right, chosen);
        cx.hint(&response, hint);
        if response.clicked() || response.drag_started() {
            cx.studies.view.selection = Some(stretch);
            super::stop_sounding(cx);
            let shown = f64::from(body.left() - HEADER) / cx.studies.view.per_second.max(1e-6);
            if from < cx.studies.view.from || to > cx.studies.view.from + shown {
                cx.studies.view.from = (from - 1.0).max(0.0);
            }
        }
        if response.drag_started()
            && let (Some(at), Some(mut take)) = (response.interact_pointer_pos(), super::selected(cx))
        {
            take.stems = vec!["vocals".to_owned()];
            take.name = Some(line.text.trim().to_owned());
            let beats = super::beats_of(cx, open, &take);
            begin_drag(cx, at, StudyDrag::Stretch(take, beats), line.text.trim().to_owned());
        }
    }
}

fn notes(ui: &mut Ui, cx: &mut Cx, open: &Arc<Open>, body: Rect) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let id = ui.scope_id().with("notes");
    let heard = &open.heard;
    // Of the selection, or of the whole recording from its first bar line.
    let (from, to) = match &cx.studies.view.selection {
        Some(selection) => (selection.from, selection.to),
        None => (open.bars.first().copied().unwrap_or(0.0), open.seconds),
    };
    let within = |start: f64| start >= from - 1e-3 && start < to;
    let chords =
        heard.harmony.as_ref().map_or(0, |harmony| harmony.chords.iter().filter(|chord| within(chord.start)).count());
    let parts = [
        (MidiPart::Bass, heard.bass.iter().filter(|note| within(note.start)).count()),
        (MidiPart::Melody, heard.melody.iter().filter(|note| within(note.start)).count()),
        (MidiPart::Chords, chords),
        (MidiPart::Groove, heard.groove.iter().filter(|hit| within(hit.time)).count()),
    ];
    let of = if cx.studies.view.selection.is_some() { "OF THE SELECTION" } else { "OF THE WHOLE RECORDING" };
    let head = Rect::from_min_size(body.min + vec2(10.0, 0.0), vec2(body.width() - 20.0, SIDE_ROW));
    text::paint(ui.painter(), head, Align2::LEFT_CENTER, theme.text.micro, of, theme.ink_faint);
    let grid = &heard.grid;
    let beats = {
        let beats = grid.beat_at(to) - grid.beat_at(from);
        if (beats - beats.round()).abs() < 0.05 { beats.round() } else { beats.ceil() }
    };
    for (index, (part, count)) in parts.into_iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(body.left(), head.bottom() + index as f32 * SIDE_ROW),
            vec2(body.width(), SIDE_ROW),
        );
        let right = if count == 1 { "1 NOTE".to_owned() } else { format!("{count} NOTES") };
        let hint = "Drag onto an instrument track to write it as notes; a groove asks the track\u{2019}s kit for its drums by name";
        let response = side_row(ui, cx, rect, id.with(index), part.name(), &right, false);
        cx.hint(&response, hint);
        if response.drag_started()
            && count > 0
            && let Some(at) = response.interact_pointer_pos()
        {
            let drag = StudyDrag::Notes { study: open.id.clone(), part, from, to, beats };
            begin_drag(cx, at, drag, format!("{} ({})", part.name(), open.title));
        }
    }

    // What is taken of the recording as a whole, from the bar the edit cursor is in.
    let mut top = head.bottom() + 4.0 * SIDE_ROW + 10.0;
    let rule = Rect::from_min_size(pos2(body.left() + 10.0, top), vec2(body.width() - 20.0, SIDE_ROW));
    text::paint(
        ui.painter(),
        rule,
        Align2::LEFT_CENTER,
        theme.text.micro,
        "INTO THE SONG, FROM THE BAR OF THE EDIT CURSOR",
        theme.ink_faint,
    );
    top += SIDE_ROW;
    let mut cursor = body.left() + 10.0;
    for (what, label, name, about) in [
        (
            Whole::Tempo,
            "TEMPO",
            "Take the tempo",
            "The recording\u{2019}s tempo becomes the song\u{2019}s, bar by bar: nothing in the song moves",
        ),
        (Whole::Parts, "PARTS", "Take the parts", "The recording\u{2019}s parts become markers of the song"),
        (Whole::Key, "KEY", "Take the key", "The song is said to be in the recording\u{2019}s key"),
        (
            Whole::Reference,
            "REFERENCE",
            "Take the recording as a reference",
            "The recording beside the song: silent unless listened to, and in no render",
        ),
        (Whole::Kit, "KIT", "Take a kit", "A drum rack of the cleanest hit of each drum, on a track of its own"),
    ] {
        let chip = Chip::action(label).named(name);
        if cursor + chip.width(ui, theme) > body.right() - 8.0 {
            (cursor, top) = (body.left() + 10.0, top + theme.metric.chip + 4.0);
        }
        let row = Rect::from_min_size(pos2(body.left(), top), vec2(body.width(), theme.metric.chip));
        let response = next_chip(ui, theme, &mut cursor, row, id.with(("whole", label)), &chip);
        cx.hint(&response, about);
        if response.clicked() {
            super::take(&ctx, cx, what);
        }
    }
}

/// A sentence in place of a list that has nothing in it.
fn empty(ui: &mut Ui, cx: &mut Cx, body: Rect, words: &str) {
    let theme = cx.theme;
    let rect = Rect::from_min_size(body.min + vec2(10.0, 6.0), vec2(body.width() - 20.0, 40.0));
    let response = ui.interact(rect, ui.scope_id().with("empty"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    let galley = trill_noir::prose::plain(ui, theme, theme.text.body, theme.ink_faint, words, rect.width());
    trill_noir::prose::paint(ui, rect.min, galley);
}
