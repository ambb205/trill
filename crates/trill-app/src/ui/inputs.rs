//! What a track records from the audio device and how it is heard
//! meanwhile: the controls a header and a mixer strip share, the line that
//! says a microphone is live, and the recordings the song no longer uses.
//!
//! The session decides everything: what a track records is the song's
//! ([`Project::set_track_input`]), the input is open only while something
//! needs it, and a take is put where it was sung. The window shows what
//! there is and adds its manners:
//!
//! * the input is chosen from a menu on the track, by channel or by pair;
//! * the monitor switch says beside it how late the input is heard, and
//!   whether that figure was measured or is what the devices say;
//! * the level of what arrives is beside the track's own, with a lamp that
//!   stays lit once a channel has been at full scale, until it is clicked;
//! * a line in the status bar says so for as long as the input is open:
//!   a microphone that is open hears the room, whatever is done with it;
//! * recordings are never deleted without the question being asked.

use super::snapshot::{MeterState, Snapshot};
use super::state::{Cx, UiState};
use super::{audio_settings, units, widgets};
use eframe::egui::{self, Align2, Color32, Id, Rect, Sense, Ui, pos2, vec2};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use trill_core::{Monitoring, Track, TrackId, TrackInput, TrackKind};
use trill_engine::{Correction, Recorded, Removed};
use trill_noir::{Chip, MenuItem, Variant, paint, text};
use trill_runtime::UnusedRecordings;

/// Channels a menu offers at the most: an interface with more is still
/// reached by a tool, and a menu of sixty lines is no menu.
const CHANNELS_OFFERED: u16 = 16;
/// Width of the list of unused recordings.
const WIDTH: f32 = 560.0;
const ROW: f32 = 20.0;
/// More recordings than this scroll.
const ROWS: f32 = 12.0;

/// Whether `track` can record an input: it holds clips and is part of the
/// song.
pub fn can_record(track: &Track) -> bool {
    track.kind == TrackKind::Normal && track.reference.is_none()
}

/// What a track records, in the few letters a header has room for.
pub fn short(input: TrackInput) -> String {
    match input.stereo {
        true => format!("In {}+{}", input.channel, input.channel + 1),
        false => format!("In {}", input.channel),
    }
}

/// How a way of hearing the input reads in a menu.
fn monitor_name(monitor: Monitoring) -> &'static str {
    match monitor {
        Monitoring::Off => "Not through Trill",
        Monitoring::Armed => "While the track is armed",
        Monitoring::Always => "Always",
    }
}

/// How many channels the input device has, as far as the window knows:
/// the open input's, or the chosen device's among the ones listed, or two.
/// A list is asked for where there is none yet, off this thread.
pub fn channels_offered(cx: &mut Cx) -> u16 {
    if let Some(open) = &cx.live.input.open {
        return open.channels.clamp(1, CHANNELS_OFFERED);
    }
    match audio_settings::input_device(cx) {
        Some(device) => device.channels.clamp(1, CHANNELS_OFFERED),
        None => 2,
    }
}

/// How late a monitored input is heard, and whether that was measured:
/// known while the input is open.
pub fn round_trip(cx: &Cx) -> Option<(f64, Correction)> {
    cx.live.input.latency.map(|latency| (latency.round_trip_ms, latency.correction))
}

/// The round trip in words: `"12.4 ms, as the devices say"`.
pub fn round_trip_words(milliseconds: f64, correction: Correction) -> String {
    let from = match correction {
        Correction::None => "as the devices say, not measured",
        Correction::Measured => "as measured",
        Correction::ByHand => "as set by hand",
    };
    format!("{milliseconds:.1} ms, {from}")
}

pub fn set_input(cx: &mut Cx, track: TrackId, input: Option<TrackInput>) {
    cx.edit(|project| project.set_track_input(track, input));
}

fn set_monitor(cx: &mut Cx, track: TrackId, input: TrackInput, monitor: Monitoring) {
    set_input(cx, track, Some(TrackInput { monitor, ..input }));
}

/// The menu of what `track` records and how it is heard, offering `most`
/// channels of the device at the most.
pub fn menu(ui: &mut Ui, cx: &mut Cx, track: &Track, most: u16) {
    let theme = cx.theme;
    ui.set_min_width(230.0);
    let id = track.id;
    let now = track.input;
    trill_noir::menu_heading(ui, theme, "Records");
    let detail = "Notes: a MIDI keyboard, or the computer's";
    if MenuItem::new("What a keyboard plays").checked(now.is_none()).detail(detail).show(ui, theme).clicked() {
        set_input(cx, id, None);
    }
    let channels = channels_offered(cx).max(now.map_or(0, |input| input.channel + u16::from(input.stereo)));
    let monitor = now.map_or(Monitoring::Off, |input| input.monitor);
    let device = cx.live.input.open.as_ref().map(|open| open.device.clone());
    trill_noir::menu_separator(ui, theme);
    let channels = channels.min(most.min(CHANNELS_OFFERED));
    for channel in 1..=channels {
        let chosen = now.is_some_and(|input| input.channel == channel && !input.stereo);
        if MenuItem::new(&format!("Input {channel}")).checked(chosen).show(ui, theme).clicked() {
            set_input(cx, id, Some(TrackInput { channel, stereo: false, monitor }));
        }
    }
    for left in (1..channels).step_by(2) {
        let chosen = now.is_some_and(|input| input.channel == left && input.stereo);
        let label = format!("Inputs {left} and {}", left + 1);
        if MenuItem::new(&label).checked(chosen).detail("As left and right").show(ui, theme).clicked() {
            set_input(cx, id, Some(TrackInput { channel: left, stereo: true, monitor }));
        }
    }
    let Some(input) = now else { return };
    trill_noir::menu_separator(ui, theme);
    trill_noir::menu_heading(ui, theme, "Heard through this track");
    if cx.live.input.held.contains(&id) {
        let detail = "Held: nobody has switched it on at this window since the song was opened";
        if MenuItem::new("Let it be heard").detail(detail).show(ui, theme).clicked() {
            cx.try_session(|session| session.let_input_be_heard(id));
        }
    }
    for choice in [Monitoring::Off, Monitoring::Armed, Monitoring::Always] {
        if MenuItem::new(monitor_name(choice)).checked(input.monitor == choice).show(ui, theme).clicked() {
            set_monitor(cx, id, input, choice);
        }
    }
    // What the device is, and how late it is heard: known once it is open.
    let about = match (device, round_trip(cx)) {
        (Some(device), Some((ms, correction))) => {
            format!("{device} \u{00B7} heard {}", round_trip_words(ms, correction))
        }
        (Some(device), None) => device,
        (None, _) => "The input opens when the track is armed or heard".to_owned(),
    };
    trill_noir::menu_separator(ui, theme);
    trill_noir::menu_heading(ui, theme, &about);
}

/// What a track records, written where a header names its kind: a button
/// that opens the menu. With the monitor on it says beside the switch how
/// late the input is heard. Returns the width it took.
pub fn badge(ui: &mut Ui, cx: &mut Cx, track: &Track, input: TrackInput, room: Rect, id: Id, quiet: Color32) -> f32 {
    let theme = cx.theme;
    let heard = input.monitor != Monitoring::Off;
    let late = round_trip(cx).filter(|_| heard);
    // A header has room for which input it is; how late it is heard is on
    // the switch beside this.
    let words = short(input);
    // An input the device has not got is said in the color of a warning.
    let lacking =
        cx.live.input.open.as_ref().is_some_and(|open| input.channel + u16::from(input.stereo) > open.channels);
    let ink = if lacking { theme.warn } else { quiet };
    let width = text::paint(ui.painter(), room, Align2::RIGHT_CENTER, theme.text.micro, &words, ink);
    let rect =
        Rect::from_min_max(pos2(room.right() - width - 2.0, room.top()), pos2(room.right() + 2.0, room.bottom()));
    let response = ui.interact(rect, id, Sense::click());
    let name = format!("Input of {}: {}", track.name, input.said());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
    if response.hovered() {
        paint::hline(ui.painter(), rect.x_range(), rect.bottom() - 2.0, ink);
    }
    let about = match (lacking, late) {
        (true, _) => cx.live.input.problem.clone().unwrap_or_else(|| "The input device has not got this input".into()),
        (false, Some((ms, correction))) => format!(
            "Records {} \u{00B7} heard through this track {} \u{00B7} click to change",
            input.said(),
            round_trip_words(ms, correction)
        ),
        (false, None) => format!("Records {} of the audio device \u{00B7} click to change", input.said()),
    };
    cx.hint(&response, about);
    egui::Popup::menu(&response).gap(1.0).show(|ui| menu(ui, cx, track, CHANNELS_OFFERED));
    width
}

/// The row of a mixer strip that says what its track records: a button
/// that opens the choice and, where the track records an input, the
/// monitor switch beside it.
pub fn strip_row(ui: &mut Ui, cx: &mut Cx, track: &Track, rect: Rect, id: Id, quiet: Color32) {
    let theme = cx.theme;
    let switch = 20.0;
    let (chooser, monitor) = match track.input {
        Some(_) => (
            Rect::from_min_max(rect.min, pos2(rect.right() - switch - theme.metric.gap, rect.bottom())),
            Some(Rect::from_min_max(pos2(rect.right() - switch, rect.top()), rect.max)),
        ),
        None => (rect, None),
    };
    // How late a monitored input is heard is on the switch beside this.
    let late = track.input.filter(|input| input.monitor != Monitoring::Off).and_then(|_| round_trip(cx));
    let label = track.input.map_or_else(|| "Keys".to_owned(), short);
    let name = match track.input {
        Some(input) => format!("Input of {} in the mixer: {}", track.name, input.said()),
        None => format!("Input of {} in the mixer: a keyboard", track.name),
    };
    let chip = Chip::new(&label, false).quiet(quiet).named(&name);
    let response = trill_noir::chip(ui, theme, chooser, id.with("choose"), &chip);
    let about = match (track.input, late) {
        (None, _) => "Records what a keyboard plays \u{00B7} click to record an input of the audio device".to_owned(),
        (Some(input), Some((ms, correction))) => {
            format!(
                "Records {} \u{00B7} heard {} \u{00B7} click to change",
                input.said(),
                round_trip_words(ms, correction)
            )
        }
        (Some(input), None) => format!("Records {} of the audio device \u{00B7} click to change", input.said()),
    };
    cx.hint(&response, about);
    egui::Popup::menu(&response).gap(1.0).show(|ui| menu(ui, cx, track, CHANNELS_OFFERED));
    if let (Some(input), Some(monitor)) = (track.input, monitor) {
        monitor_chip(ui, cx, track, input, monitor, id.with("monitor"), quiet, " in the mixer");
    }
}

/// The monitor switch of a track that records an input, in `rect`: a click
/// switches between not heard and heard while armed; "always" is in the
/// input's menu, and shows here in another color.
#[expect(clippy::too_many_arguments, reason = "one switch drawn in two places; a struct would only rename them")]
pub fn monitor_chip(
    ui: &Ui,
    cx: &mut Cx,
    track: &Track,
    input: TrackInput,
    rect: Rect,
    id: Id,
    quiet: Color32,
    place: &str,
) {
    let theme = cx.theme;
    if cx.live.input.held.contains(&track.id) {
        // The song says the input is heard, and nobody at this window
        // switched that on: it is held, and this click lets it go.
        let name = format!("Monitor {}{place}, held", track.name);
        let chip = Chip::new("I", false).accent(theme.warn).quiet(theme.warn).named(&name);
        let response = trill_noir::chip(ui, theme, rect, id, &chip);
        paint::frame(ui.painter(), rect, theme.warn);
        cx.hint(&response, held_words(input));
        if response.clicked() {
            let id = track.id;
            cx.try_session(|session| session.let_input_be_heard(id));
        }
        return;
    }
    let accent = if input.monitor == Monitoring::Always { theme.info } else { theme.ok };
    let name = format!("Monitor {}{place}", track.name);
    // On, the switch says how late the input is heard, in milliseconds,
    // as soon as that is known: a number a singer wants in sight.
    let on = input.monitor != Monitoring::Off;
    let figure = round_trip(cx).filter(|(ms, _)| on && *ms < 999.5).map(|(ms, _)| format!("{ms:.0}"));
    let label = figure.as_deref().unwrap_or("I");
    let chip = Chip::new(label, on).accent(accent).quiet(quiet).named(&name);
    let response = trill_noir::chip(ui, theme, rect, id, &chip);
    let late = round_trip(cx).map(|(ms, correction)| format!(" {}", round_trip_words(ms, correction)));
    let about = match input.monitor {
        Monitoring::Off => "Monitor: hear the input through this track while it is armed \u{00B7} click to switch on \
                            (use headphones: a microphone that hears the speakers feeds back)"
            .to_owned(),
        Monitoring::Armed => format!(
            "Heard through this track while it is armed{} \u{00B7} click to switch off",
            late.map(|late| format!(",{late}")).unwrap_or_default()
        ),
        Monitoring::Always => format!(
            "Always heard through this track{} \u{00B7} click to switch off",
            late.map(|late| format!(",{late}")).unwrap_or_default()
        ),
    };
    cx.hint(&response, about);
    if response.clicked() {
        let next = if input.monitor == Monitoring::Off { Monitoring::Armed } else { Monitoring::Off };
        set_monitor(cx, track.id, input, next);
    }
}

/// What the pointer is told over the switch of an input that is held.
fn held_words(input: TrackInput) -> String {
    let says = match input.monitor {
        Monitoring::Always => "always",
        _ => "while the track is armed",
    };
    format!(
        "Held: the song says {} is heard through this track {says}, which opens the microphone into the speakers, and \
         nobody has switched that on at this window since the song was opened \u{00B7} click to let it be heard",
        input.said()
    )
}

/// A sentence as the session words it, with a capital.
pub fn capitalized(words: &str) -> String {
    let mut letters = words.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// The level of what `input` hears, in `rect`: a meter with a lamp over it
/// that stays lit once the input has been at full scale. A click puts the
/// lamps out. Nothing is drawn while the input is closed.
pub fn meter(ui: &Ui, cx: &mut Cx, track: &Track, input: TrackInput, rect: Rect, id: Id) {
    if cx.live.input.open.is_none() {
        return;
    }
    let theme = cx.theme;
    let state = cx.live.input.meter(input);
    let painter = ui.painter();
    let lamp_ink = if state.clipped { theme.signal } else { theme.well };
    if rect.width() > rect.height() {
        // Lying down, as a header has room for it: one bar for the louder
        // of its channels, the lamp at its right end.
        let lamp = Rect::from_min_max(pos2(rect.right() - 5.0, rect.top()), rect.max);
        let bar = Rect::from_min_max(rect.min, pos2(lamp.left() - 2.0, rect.bottom()));
        painter.rect_filled(lamp, 0.0, lamp_ink);
        level_bar(painter, theme, bar, state);
    } else {
        let lamp = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 3.0));
        let column = Rect::from_min_max(pos2(rect.left(), lamp.bottom() + 1.0), rect.max);
        painter.rect_filled(lamp, 0.0, lamp_ink);
        trill_noir::stereo_meter(painter, theme, column, widgets::levels(state), widgets::meter_zones());
    }
    let response = ui.interact(rect, id, Sense::click());
    let level = units::format_level(state.peak_db);
    let name = match state.clipped {
        true => format!("Input level of {}: {level}, clipped", track.name),
        false => format!("Input level of {}: {level}", track.name),
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    let about = match state.clipped {
        true => format!(
            "What {} hears reached full scale: turn the input down at the device \u{00B7} click to put the lamp out",
            input.said()
        ),
        false => format!("What {} hears: {level} at its highest lately", input.said()),
    };
    cx.hint(&response, about);
    if response.clicked() {
        cx.state.clear_input_lamps = true;
    }
}

/// A level as a bar lying down: filled from the left in the colors of the
/// zones the louder channel passes through, with a line at its highest
/// lately.
pub fn level_bar(painter: &egui::Painter, theme: &trill_noir::Theme, bar: Rect, state: MeterState) {
    painter.rect_filled(bar, 0.0, theme.well);
    let levels = widgets::levels(state);
    let level = levels.left.max(levels.right);
    let zones = widgets::meter_zones();
    let x_at = |position: f32| bar.left() + bar.width() * position.clamp(0.0, 1.0);
    for (from, to, color) in
        [(0.0, zones.hot, theme.ok), (zones.hot, zones.over, theme.warn), (zones.over, 1.0, theme.signal)]
    {
        if level > from {
            painter.rect_filled(Rect::from_x_y_ranges(x_at(from)..=x_at(level.min(to)), bar.y_range()), 0.0, color);
        }
    }
    if let Some(peak) = levels.peak {
        paint::vline(painter, x_at(peak), bar.y_range(), theme.ink);
    }
}

/// That the input is open, for the status bar: a microphone that is open
/// is live. `None` while it is closed.
pub fn live_words(cx: &Cx) -> Option<String> {
    let open = cx.live.input.open.as_ref()?;
    Some(format!("Input live \u{00B7} {}", open.device))
}

/// What the pointer is told over the status bar's line.
pub fn live_hint(cx: &Cx) -> String {
    let Some(open) = &cx.live.input.open else { return String::new() };
    let why = if cx.live.take.is_some() {
        "a take is being recorded"
    } else if cx.settings.is_listening() {
        "its level is on show"
    } else {
        "a track that records it is armed or heard"
    };
    let late =
        round_trip(cx).map(|(ms, correction)| format!(" \u{00B7} round trip {}", round_trip_words(ms, correction)));
    format!(
        "{} is open because {why}: it closes when nothing needs it{} \u{00B7} click for the audio settings",
        open.device,
        late.unwrap_or_default()
    )
}

/// A take has ended: where it left more than one take to choose from, the
/// track's lanes unfold; what did not arrive is said.
pub fn take_ended(state: &mut UiState, snapshot: &Snapshot, recorded: Option<&Recorded>) {
    let Some(recorded) = recorded else { return };
    for take in &recorded.takes {
        if snapshot.track(take.track).is_some_and(|track| track.takes.len() > 1) {
            state.arrangement.takes_open.insert(take.track);
        }
    }
    if let Some(last) = recorded.takes.last() {
        state.select_track(last.track);
    }
    if recorded.silent_frames > 0 {
        state.toasts.info(format!(
            "{} of the take did not arrive and is silence where it would have been",
            samples_in_words(recorded.silent_frames)
        ));
    }
}

/// A number of frames as a listener would say it. (At the rate nearly
/// every device runs at: this is a sentence, not a measurement.)
fn samples_in_words(frames: u64) -> String {
    let milliseconds = frames as f64 / 48.0;
    if milliseconds < 1000.0 { format!("{milliseconds:.0} ms") } else { format!("{:.1} s", milliseconds / 1000.0) }
}

// ---- the recordings the song no longer uses --------------------------------

/// What a worker found or did, on its way back to the window.
#[derive(Default)]
struct Worked {
    listed: Option<UnusedRecordings>,
    removed: Option<Result<Removed, String>>,
}

/// The list of recordings nothing plays any more, while it is up. Which
/// they are is a question about every song Trill knows of, whose files a
/// worker reads: the list says so until it has come.
#[derive(Clone, Default)]
pub struct Unused {
    /// What was found; `None` while the songs are being looked through.
    pub listed: Option<UnusedRecordings>,
    /// Whether the question was asked: Trill never removes a recording by
    /// itself, and never without being told twice.
    pub asked: bool,
    /// Whether the ones listed are being removed.
    removing: bool,
    worked: Arc<Mutex<Worked>>,
}

impl Unused {
    /// True while a worker is still at it: a picture waits for it.
    pub fn is_waiting(&self) -> bool {
        self.listed.is_none() || self.removing
    }

    /// True while the ones listed are being removed.
    pub fn is_removing(&self) -> bool {
        self.removing
    }
}

/// Opens the list, and has a worker look through the songs.
pub fn open_unused(cx: &mut Cx) {
    let unused = Unused::default();
    let worked = unused.worked.clone();
    cx.state.unused_recordings = Some(unused);
    cx.sounds.quietly(move |runtime| {
        worked.lock().listed = Some(runtime.unused_recordings());
        Ok(())
    });
}

/// A size in bytes as a person reads it.
pub fn size_words(bytes: u64) -> String {
    let megabytes = bytes as f64 / 1_048_576.0;
    if megabytes >= 1024.0 {
        format!("{:.1} GB", megabytes / 1024.0)
    } else if megabytes >= 10.0 {
        format!("{megabytes:.0} MB")
    } else {
        format!("{megabytes:.1} MB")
    }
}

/// "1 recording", "3 recordings".
fn counted(count: usize) -> String {
    if count == 1 { "1 recording".to_owned() } else { format!("{count} recordings") }
}

/// The list, while it is open.
pub fn unused_window(ctx: &egui::Context, cx: &mut Cx) {
    let Some(mut unused) = cx.state.unused_recordings.clone() else { return };
    let theme = cx.theme;
    // What the worker has finished since the last frame.
    let (listed, removed) = {
        let mut worked = unused.worked.lock();
        (worked.listed.take(), worked.removed.take())
    };
    if let Some(listed) = listed {
        unused.listed = Some(listed);
    }
    let mut close = false;
    match removed {
        Some(Ok(removed)) => {
            let count = removed.recycled + removed.set_aside.len();
            cx.state.toasts.info(format!(
                "Removed {}, {}, {}",
                counted(count),
                size_words(removed.bytes),
                removed.went()
            ));
            close = true;
        }
        Some(Err(why)) => {
            cx.state.toasts.error(capitalized(&why));
            close = true;
        }
        None => {}
    }
    if unused.is_waiting() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    let (mut ask, mut remove) = (false, false);
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let line = |ui: &mut Ui, words: &str, style: trill_noir::TextStyle, ink: Color32| {
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, words));
        text::paint(ui.painter(), rect, Align2::LEFT_CENTER, style, words, ink);
        rect
    };
    let name_of =
        |path: &std::path::Path| path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let modal =
        egui::Modal::new(Id::unique("unused-recordings")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Unused recordings", theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);
            ui.add_space(8.0);
            let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(8.0);

            let small = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
            let value = trill_noir::TextStyle { size: 11.0, ..theme.text.value };
            let Some(listed) = &unused.listed else {
                line(ui, "Looking through the songs Trill knows of\u{2026}", small, theme.ink_dim);
                return;
            };
            if listed.unused.is_empty() {
                line(ui, "No recording of this song can go.", small, theme.ink);
                let about = "A recording stays while a clip or a take plays it, or undo can bring one back.";
                line(ui, about, small, theme.ink_dim);
            } else {
                let about = "Nothing plays these any more, in this song or any other Trill knows of, and no undo does.";
                line(ui, about, small, theme.ink_dim);
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt("unused").max_height(ROWS * ROW).auto_shrink([false, true]).show(
                    ui,
                    |ui| {
                        for (path, size) in &listed.unused {
                            let rect = line(ui, &name_of(path), small, theme.ink);
                            let size = size_words(*size);
                            text::paint(ui.painter(), rect, Align2::RIGHT_CENTER, value, &size, theme.ink_dim);
                        }
                    },
                );
            }
            if !listed.played_elsewhere.is_empty() {
                // Not this song's any more, and somebody else's still.
                ui.add_space(8.0);
                line(ui, "Another song plays these, so they stay:", small, theme.ink_dim);
                egui::ScrollArea::vertical()
                    .id_salt("elsewhere")
                    .max_height(4.0 * ROW)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for kept in &listed.played_elsewhere {
                            let words = format!("{} \u{00B7} played by {}", name_of(&kept.path), kept.by.join(" and "));
                            line(ui, &words, small, theme.ink_dim);
                        }
                    });
            }
            ui.add_space(6.0);
            let folder = format!("In {}", listed.folder.display());
            line(ui, &folder, theme.text.micro, theme.ink_faint);
            ui.add_space(10.0);

            let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
            let layout = egui::Layout::right_to_left(egui::Align::Center);
            let mut cell = ui.new_child(egui::UiBuilder::new().id_salt("unused-foot").max_rect(foot).layout(layout));
            cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
            let count = listed.unused.len();
            if unused.removing {
                text::paint(ui.painter(), foot, Align2::LEFT_CENTER, small, "Removing\u{2026}", theme.ink_dim);
            } else if unused.asked {
                // The question: nothing goes before it is answered, and it
                // says where what goes will be.
                let question = match count {
                    1 => format!("Remove this recording? It goes {}.", listed.goes),
                    count => format!("Remove these {count} recordings? They go {}.", listed.goes),
                };
                let (asked, said) = ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover());
                let _ = asked;
                said.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &question));
                let room = foot.with_max_x(foot.right() - 230.0);
                text::paint(ui.painter(), room, Align2::LEFT_CENTER, small, &question, theme.warn);
                if trill_noir::button(&mut cell, theme, "Remove them", Variant::Primary).clicked() {
                    remove = true;
                }
                if trill_noir::button(&mut cell, theme, "Keep them", Variant::Default).clicked() {
                    close = true;
                }
            } else {
                let total: u64 = listed.unused.iter().map(|(_, size)| size).sum();
                let note = if count == 0 {
                    String::new()
                } else {
                    format!("{} \u{00B7} {}", counted(count), size_words(total))
                };
                text::paint(ui.painter(), foot, Align2::LEFT_CENTER, theme.text.micro, &note, theme.ink_faint);
                if trill_noir::button(&mut cell, theme, "Close", Variant::Default).clicked() {
                    close = true;
                }
                if count > 0 && trill_noir::button(&mut cell, theme, "Remove\u{2026}", Variant::Default).clicked() {
                    ask = true;
                }
            }
        });
    if modal.should_close() && !unused.removing {
        close = true;
    }
    unused.asked |= ask;
    if remove && let Some(listed) = &unused.listed {
        unused.removing = true;
        let paths: Vec<PathBuf> = listed.unused.iter().map(|(path, _)| path.clone()).collect();
        let worked = unused.worked.clone();
        cx.sounds.quietly(move |runtime| {
            worked.lock().removed = Some(runtime.delete_unused_recordings(&paths));
            Ok(())
        });
    }
    cx.state.unused_recordings = (!close).then_some(unused);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_reads_short_in_a_header_and_whole_in_a_sentence() {
        let one = TrackInput::mono(3);
        let pair = TrackInput { channel: 1, stereo: true, monitor: Monitoring::Armed };
        assert_eq!((short(one), short(pair)), ("In 3".to_owned(), "In 1+2".to_owned()));
        assert_eq!(round_trip_words(12.44, Correction::None), "12.4 ms, as the devices say, not measured");
        assert_eq!(round_trip_words(58.91, Correction::Measured), "58.9 ms, as measured");
        assert_eq!(round_trip_words(7.0, Correction::ByHand), "7.0 ms, as set by hand");
    }

    #[test]
    fn sizes_and_gaps_read_as_a_person_says_them() {
        assert_eq!(size_words(512 * 1024), "0.5 MB");
        assert_eq!(size_words(690 * 1_048_576), "690 MB");
        assert_eq!(size_words(3 * 1024 * 1_048_576), "3.0 GB");
        assert_eq!(samples_in_words(480), "10 ms");
        assert_eq!(samples_in_words(96_000), "2.0 s");
    }
}
