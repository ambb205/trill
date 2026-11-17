//! What the sampled instrument plays: the instrument it has loaded.
//!
//! Which instrument a Sampled Instrument plays is not a parameter, so the
//! generic panel cannot show it. Here the device gets a column of its own,
//! in front of its parameters: the instrument's name, a map of where its
//! recordings lie over the keys and over how hard they are struck
//! ([`trill_noir::key_map`]), how much there is of it, the ways of playing
//! it switches between, and whether it is still being read from the disk.
//!
//! It takes an instrument dragged from the browser or from another
//! program (see [`crate::ui::dragging`]).

use super::devices::{self, Body};
use super::sounds::{DropTarget, size_words};
use super::state::Cx;
use super::{browser, sample_panels};
use eframe::egui::{Align2, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;
use trill_core::pitch::pitch_name;
use trill_core::{DeviceId, SampledInstrument};
use trill_noir::{Chip, KeyBox, KeyMap, fade, paint, text};

pub const SAMPLED: &str = trill_dsp::sampled::URI;

/// Width of the column that shows the instrument.
pub(super) const INSTRUMENT_COLUMN: f32 = 316.0;
const HEADING: f32 = devices::SECTION_HEADER;
/// The map is as tall as the column leaves it, within these.
const MAP_HEIGHT: (f32, f32) = (64.0, 132.0);
const LINE: f32 = 15.0;

/// What a snapshot keeps of a device's instrument, for drawing it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    /// What the device says it has loaded; `None` for a device with no
    /// instrument, or one whose instrument could not be read.
    pub said: Option<Arc<SampledInstrument>>,
    /// The instrument's regions as the map draws them, each once.
    pub boxes: Vec<KeyBox>,
    /// The keys that choose a way of playing.
    pub switches: Vec<u8>,
    /// What is wrong with it that it plays in spite of, or instead of.
    pub problems: Vec<String>,
}

impl Shown {
    pub fn of(said: Option<Arc<SampledInstrument>>, problems: Vec<String>) -> Self {
        let mut boxes: Vec<KeyBox> = said
            .iter()
            .flat_map(|said| said.regions.iter())
            .map(|region| KeyBox { keys: region.keys, levels: region.velocities, back: region.release })
            .collect();
        // What sounds for a key coming up lies behind what the key plays.
        boxes.sort_by_key(|item| (!item.back, item.keys, item.levels));
        boxes.dedup();
        let switches =
            said.iter().flat_map(|said| said.profile.articulations.iter()).filter_map(|way| way.key).collect();
        Self { said, boxes, switches, problems }
    }
}

/// What there is of an instrument, in a line: `637 recordings · 712 MB ·
/// 16 layers · 4 takes`.
fn amount_words(said: &SampledInstrument) -> String {
    let mut words = vec![
        format!("{} recording{}", said.recordings, if said.recordings == 1 { "" } else { "s" }),
        size_words(said.bytes),
    ];
    if said.layers > 1 {
        words.push(format!("{} layers", said.layers));
    }
    if said.takes > 1 {
        words.push(format!("{} takes", said.takes));
    }
    words.join(" \u{00B7} ")
}

/// Where it plays and how: `A0–C8 · natural, retuned`, `Drums C2–B3`.
fn playing_words(said: &SampledInstrument) -> String {
    let profile = &said.profile;
    let mut words = Vec::new();
    let drums = profile.role == Some(trill_core::Role::Drums);
    let range = profile.range.or_else(|| {
        let keys = said.regions.iter().filter(|region| !region.release);
        keys.fold(None, |range: Option<(u8, u8)>, region| {
            Some(range.map_or(region.keys, |(low, high)| (low.min(region.keys.0), high.max(region.keys.1))))
        })
    });
    if let Some((low, high)) = range {
        let keys = format!("{}\u{2013}{}", pitch_name(low), pitch_name(high));
        words.push(if drums { format!("Drums {keys}") } else { keys });
    }
    let ways: Vec<&str> = profile.articulations.iter().map(|way| way.name.as_str()).collect();
    if !ways.is_empty() {
        words.push(ways.join(", ").replace('_', " "));
    }
    words.join(" \u{00B7} ")
}

/// The column of a Sampled Instrument that shows its instrument.
pub(super) fn instrument(ui: &mut Ui, cx: &mut Cx, body: &Body, column: Rect, panel: Rect) {
    let theme = cx.theme;
    let device = body.device.id;
    let id = body.id.with("instrument");
    let shown = cx.snap.instruments.get(&device).cloned().unwrap_or_default();
    let reading = cx.live.reading.get(&device).copied();
    let gap = theme.metric.gap;
    let painter = ui.painter().clone();
    let heading = Rect::from_min_size(column.min, vec2(column.width(), HEADING));
    let mut top = sample_panels::heading(ui, cx, heading, "Instrument");

    // Its name. (Another instrument takes its place; nothing takes it away
    // but removing the device.)
    let name_row = Rect::from_min_size(pos2(column.left(), top), vec2(column.width(), 18.0));
    let ink = if body.dimmed { fade(theme.ink, theme.disabled) } else { theme.ink };
    match &shown.said {
        Some(said) => {
            text::paint_name(&painter, name_row, Align2::LEFT_CENTER, theme.text.body, &said.name, ink);
        }
        None => {
            text::paint(&painter, name_row, Align2::LEFT_CENTER, theme.text.body, "No instrument", theme.ink_faint);
        }
    }
    top = name_row.bottom() + gap;

    // The map: what plays where. As tall as the lines under it leave it.
    let under = 3.0 * LINE + 2.0 * gap + theme.metric.chip + gap;
    let height = (column.bottom() - top - under).clamp(MAP_HEIGHT.0, MAP_HEIGHT.1);
    let map_rect = Rect::from_min_size(pos2(column.left(), top), vec2(column.width(), height));
    let accent = if body.dimmed { fade(body.accent, theme.disabled) } else { body.accent };
    let range = shown.boxes.iter().filter(|item| !item.back).fold(None, |range: Option<(u8, u8)>, item| {
        Some(range.map_or(item.keys, |(low, high)| (low.min(item.keys.0), high.max(item.keys.1))))
    });
    // A piano's keys where there is nothing to show yet.
    let range = match (range, shown.switches.iter().min(), shown.switches.iter().max()) {
        (Some((low, high)), Some(first), Some(last)) => (low.min(*first), high.max(*last)),
        (Some(range), _, _) => range,
        (None, _, _) => (21, 108),
    };
    let map = KeyMap { boxes: &shown.boxes, range, marks: &shown.switches, accent };
    trill_noir::key_map(&painter, theme, map_rect, &map);
    let response = ui.interact(map_rect, id.with("map"), Sense::hover());
    // How much there is of it, and where and how it plays.
    let described = shown.said.as_ref().map(|said| (amount_words(said), playing_words(said)));
    response.widget_info(|| {
        let label = match (&shown.said, &described) {
            (Some(said), Some((amount, playing))) => format!("Instrument {}: {amount}, {playing}", said.name),
            _ => "No instrument".to_string(),
        };
        WidgetInfo::labeled(Role::Image, true, label)
    });
    match (response.hover_pos().and_then(|pointer| map.at(map_rect, pointer)), &shown.said) {
        (Some((key, level)), Some(_)) => {
            let here = |item: &&KeyBox| {
                (item.keys.0..=item.keys.1).contains(&key) && (item.levels.0..=item.levels.1).contains(&level)
            };
            let playing = shown.boxes.iter().filter(|item| !item.back).filter(here).count();
            let what = match playing {
                0 => "nothing is recorded here".to_string(),
                1 => "one recording".to_string(),
                more => format!("{more} recordings, in layers or as ways of playing"),
            };
            cx.hint(&response, format!("{} struck at {level}: {what}", pitch_name(key)));
        }
        (_, Some(_)) => cx.hint(
            &response,
            "Where the recordings lie: keys across, how hard they are struck upwards \u{00B7} lit keys choose a way of playing",
        ),
        (_, None) => cx.hint(&response, "Drop an instrument (.sfz) here, or use Load"),
    }
    if shown.said.is_none() {
        let hint = map.boxes_area(map_rect);
        let words = "Drop an instrument here";
        text::paint(&painter, hint, Align2::CENTER_CENTER, theme.text.micro, words, theme.ink_faint);
    }
    top = map_rect.bottom() + gap;

    // How much there is of it, where it plays, and how it is doing.
    let line = |top: f32| Rect::from_min_size(pos2(column.left(), top), vec2(column.width(), LINE));
    if let Some((amount, playing)) = &described {
        text::paint(&painter, line(top), Align2::LEFT_CENTER, theme.text.micro, amount, theme.ink_dim);
        text::paint_name(&painter, line(top + LINE), Align2::LEFT_CENTER, theme.text.micro, playing, theme.ink_faint);
    }
    let status = line(top + 2.0 * LINE);
    let state = ui.interact(status, id.with("status"), Sense::hover());
    let loading = reading.filter(|reading| reading.is_loading());
    match (&shown.said, shown.problems.first(), loading) {
        (_, Some(problem), _) => {
            state.widget_info(|| WidgetInfo::labeled(Role::Label, true, problem.clone()));
            text::paint_name(&painter, status, Align2::LEFT_CENTER, theme.text.micro, problem, theme.warn);
            cx.hint(&state, shown.problems.join(" \u{00B7} "));
        }
        (Some(_), None, Some(reading)) => {
            let words = format!("Reading {} of {} recordings", reading.ready, reading.of);
            state.widget_info(|| WidgetInfo::labeled(Role::Label, true, words.clone()));
            // A bar under the words, as far along as the reading is.
            let bar = Rect::from_min_max(pos2(status.left(), status.bottom() - 2.0), status.max);
            let done = reading.ready as f32 / reading.of.max(1) as f32;
            paint::progress(&painter, bar, done, theme.well, theme.signal);
            text::paint(&painter, status, Align2::LEFT_CENTER, theme.text.micro, &words, theme.signal);
            cx.hint(
                &state,
                "Notes whose recording is not read yet are silent \u{00B7} a render waits for what it plays",
            );
        }
        (Some(_), None, None) if reading.is_some_and(|reading| reading.unreadable > 0) => {
            let unreadable = reading.map_or(0, |reading| reading.unreadable);
            let words = format!("{unreadable} of its recordings could not be read");
            state.widget_info(|| WidgetInfo::labeled(Role::Label, true, words.clone()));
            text::paint(&painter, status, Align2::LEFT_CENTER, theme.text.micro, &words, theme.warn);
            cx.hint(
                &state,
                "Their files are there and are not audio this reads \u{00B7} the notes that play them are silent",
            );
        }
        (Some(said), None, None) => match &said.unsupported {
            Some(unsupported) => {
                let words = "Not everything its file asks for is done";
                state.widget_info(|| WidgetInfo::labeled(Role::Label, true, unsupported.clone()));
                text::paint(&painter, status, Align2::LEFT_CENTER, theme.text.micro, words, theme.ink_faint);
                cx.hint(&state, unsupported.clone());
                state.on_hover_text(unsupported);
            }
            None => {
                let words = "Plays everything its file asks for";
                text::paint(&painter, status, Align2::LEFT_CENTER, theme.text.micro, words, theme.ink_faint);
            }
        },
        (None, None, _) => {}
    }
    top = status.bottom() + gap + 2.0;

    let mut x = column.left();
    let mut action = |ui: &Ui, label: &str, enabled: bool| {
        let spec = Chip::action(label).dimmed(!enabled);
        let rect = Rect::from_min_size(pos2(x, top), vec2(spec.width(ui, theme), theme.metric.chip));
        x = rect.right() + gap - 1.0;
        trill_noir::chip(ui, theme, rect, id.with(label), &spec)
    };
    let file = shown.said.as_ref().map(|said| said.file.clone());
    let response = action(ui, "Load", true);
    cx.hint(&response, "Choose an instrument's file (.sfz) for the device to play");
    if response.clicked() {
        super::files::load_preset_file(&ui.ctx().clone(), cx, device);
    }
    let response = action(ui, "Reveal", file.is_some());
    cx.hint(&response, "Show the instrument's file in the file manager");
    if response.clicked()
        && let Some(file) = &file
    {
        browser::reveal(cx, PathBuf::from(file));
    }

    // An instrument dragged anywhere over the panel replaces this one.
    if offer(cx, panel.intersect(ui.clip_rect()), device) {
        paint::frame(ui.painter(), map_rect, theme.ink);
    }
}

/// Offers the device to a drag that is over `rect`. Returns whether the
/// pointer is there with something the device can take.
fn offer(cx: &mut Cx, rect: Rect, device: DeviceId) -> bool {
    let Some(drag) = &mut cx.state.sound_drag else { return false };
    if !drag.pointer.is_some_and(|pointer| rect.contains(pointer)) {
        return false;
    }
    drag.offer(DropTarget::Instrument { device });
    drag.target.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::{Articulation, InstrumentProfile, SampledRegion};

    #[test]
    fn what_there_is_of_an_instrument_is_said_in_two_lines() {
        let region = |keys, velocities, release| SampledRegion { keys, velocities, root: 60, switch: None, release };
        let piano = SampledInstrument {
            name: "Small Grand".into(),
            recordings: 637,
            bytes: 746_586_112,
            layers: 16,
            takes: 4,
            regions: vec![
                region((21, 23), (1, 40), false),
                region((21, 23), (1, 40), false),
                region((0, 127), (0, 127), true),
            ],
            profile: InstrumentProfile {
                range: Some((21, 108)),
                articulations: vec![Articulation::key("natural", 12).as_default(), Articulation::key("re_tuned", 13)],
                ..InstrumentProfile::default()
            },
            ..SampledInstrument::default()
        };
        assert_eq!(amount_words(&piano), "637 recordings \u{00B7} 712 MB \u{00B7} 16 layers \u{00B7} 4 takes");
        assert_eq!(playing_words(&piano), "A0\u{2013}C8 \u{00B7} natural, re tuned");
        let shown = Shown::of(Some(Arc::new(piano)), Vec::new());
        assert_eq!(shown.boxes.len(), 2, "a region that lies where another does is drawn once");
        assert!(shown.boxes[0].back && !shown.boxes[1].back, "what sounds for a key coming up lies behind");
        assert_eq!(shown.switches, [12, 13]);

        let kit = SampledInstrument {
            recordings: 1,
            bytes: 300_000,
            regions: vec![region((36, 36), (0, 127), false), region((59, 59), (0, 127), false)],
            profile: InstrumentProfile { role: Some(trill_core::Role::Drums), ..InstrumentProfile::default() },
            ..SampledInstrument::default()
        };
        assert_eq!(amount_words(&kit), "1 recording \u{00B7} 293 KB");
        assert_eq!(playing_words(&kit), "Drums C2\u{2013}B3", "the keys its regions cover, where it says no range");
        assert_eq!(Shown::default(), Shown::of(None, Vec::new()));
    }
}
