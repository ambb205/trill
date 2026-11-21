//! The input's part of the Audio and MIDI panel: which device is recorded
//! from, what is arriving on it, and how long sound takes to go round.
//!
//! **The input is opened by a hand.** The panel does not open a microphone
//! by being looked at: its level shows when the switch beside it is put
//! on, and the input closes with the panel unless a track needs it.
//!
//! **A take is put where it was sung** by taking the round trip off: what
//! the output says, what the input says, and what handing the sound across
//! takes. What devices say of themselves is often short of the truth, so
//! the panel measures it: a second of quiet noise is played and listened
//! for, through a cable or a microphone held to the speaker. What it finds
//! is offered, never taken by itself; once taken it is kept for this pair
//! of devices as they are open, and can be put right by hand.

use super::{GAP, ROW, WIDTH, choice, heading, list_devices, paragraph};
use crate::ui::state::Cx;
use crate::ui::{inputs, units, widgets};
use eframe::egui::{self, Align2, Id, Rect, Sense, Ui, pos2, vec2};
use trill_engine::{AudioConfig, AudioDevice, AudioDevices, Correction, Measured, Measurement, RecordingLatency};
use trill_noir::{Chip, MenuItem, TextStyle, paint, text};

/// How far one press of a nudge moves the correction by hand, in
/// milliseconds: about a twentieth of the shortest note anyone plays.
const NUDGE_MS: f64 = 0.5;
/// Height of the row of level bars.
const LEVELS: f32 = 30.0;

/// What the panel keeps of the input between frames.
#[derive(Default)]
pub struct InputPanel {
    /// Whether the panel is showing the input's level, and so holding the
    /// input open.
    listening: bool,
    /// The channel the measurement listens on, counted from 1.
    channel: u16,
}

impl InputPanel {
    pub(super) fn is_listening(&self) -> bool {
        self.listening
    }
}

/// The input device the configuration in use names, among the devices as
/// last listed; the system's default where it names none. A list is asked
/// for where there is none yet.
pub fn input_device(cx: &mut Cx) -> Option<AudioDevice> {
    if cx.settings.devices.is_none() {
        list_devices(cx);
    }
    chosen(&cx.audio.config, cx.settings.devices.as_ref()?).cloned()
}

fn chosen<'a>(config: &AudioConfig, devices: &'a AudioDevices) -> Option<&'a AudioDevice> {
    match &config.input {
        Some(name) => devices.inputs.iter().find(|device| device.name == *name),
        None => devices.inputs.iter().find(|device| device.is_default).or(devices.inputs.first()),
    }
}

/// How the draft's input device reads in the form.
fn device_label(draft: &AudioConfig, devices: Option<&AudioDevices>) -> String {
    match (&draft.input, devices) {
        (None, Some(devices)) => match chosen(draft, devices) {
            Some(device) => format!("System default \u{00B7} {}", device.name),
            None => "None connected".to_owned(),
        },
        (None, None) => "System default".to_owned(),
        (Some(name), Some(devices)) if !devices.inputs.iter().any(|device| device.name == *name) => {
            format!("{name} \u{00B7} not connected")
        }
        (Some(name), _) => name.clone(),
    }
}

/// The row of the form that chooses the device recorded from. It edits the
/// draft, like the rows around it.
pub(super) fn device_row(ui: &mut Ui, cx: &mut Cx, draft: &mut AudioConfig, devices: Option<&AudioDevices>, id: Id) {
    let theme = cx.theme;
    let busy = cx.settings.applying;
    let (line, _) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let label = device_label(draft, devices);
    let response = choice(ui, theme, line, id.with("input-device"), "Input", &label, busy);
    cx.hint(
        &response,
        "The device Trill records from: opened only while a track, a measurement or this panel needs it",
    );
    if busy {
        return;
    }
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(line.width());
        let default = AudioConfig { input: None, ..draft.clone() };
        let named = device_label(&default, devices);
        if MenuItem::new(&named).checked(draft.input.is_none()).show(ui, theme).clicked() {
            draft.input = None;
        }
        let inputs = devices.map(|devices| devices.inputs.as_slice()).unwrap_or_default();
        if !inputs.is_empty() {
            trill_noir::menu_separator(ui, theme);
        }
        for device in inputs {
            let current = draft.input.as_deref() == Some(device.name.as_str());
            let channels =
                if device.channels == 1 { "1 channel".to_owned() } else { format!("{} channels", device.channels) };
            if MenuItem::new(&device.name).checked(current).detail(&channels).show(ui, theme).clicked() {
                draft.input = Some(device.name.clone());
            }
        }
    });
}

/// Says to the session that the panel watches the input's level, or has
/// stopped. The input is open for as long as somebody watches.
pub(super) fn listen(cx: &mut Cx, on: bool) {
    if std::mem::replace(&mut cx.settings.input.listening, on) != on {
        cx.with_session(|session| {
            session.watch_input(on);
            // Open now, in place of at the next round of chores: the
            // switch was just pressed. Why it could not be is said below.
            if on {
                let _ = session.ready_input();
            }
        });
    }
}

/// The panel goes: it no longer holds the input open.
pub(super) fn closed(cx: &mut Cx) {
    listen(cx, false);
}

/// What is open, on one line.
fn open_line(cx: &Cx) -> Option<String> {
    let open = cx.live.input.open.as_ref()?;
    let sharing = if open.exclusive { "exclusive" } else { "shared" };
    let channels = if open.channels == 1 { "1 channel".to_owned() } else { format!("{} channels", open.channels) };
    let clock = match (open.shares_clock, cx.live.input.latency) {
        (true, _) => "on the output\u{2019}s clock".to_owned(),
        (false, Some(latency)) => format!("on a clock of its own, {}", drift_words(latency.drift_ppm)),
        (false, None) => "on a clock of its own".to_owned(),
    };
    Some(format!(
        "{} \u{00B7} {channels} \u{00B7} {} \u{00B7} {sharing} \u{00B7} {clock}",
        open.device,
        super::format_rate(f64::from(open.sample_rate))
    ))
}

/// How far an input's clock runs from the output's, as it is read now: a
/// figure that settles over the first seconds an input is open.
fn drift_words(parts_in_a_million: f64) -> String {
    let size = parts_in_a_million.abs();
    if size < 0.05 {
        "in step with the output\u{2019}s".to_owned()
    } else if parts_in_a_million > 0.0 {
        format!("{size:.1} parts in a million fast")
    } else {
        format!("{size:.1} parts in a million slow")
    }
}

/// The figures a round trip is made of, on one line.
fn figures(latency: &RecordingLatency) -> String {
    let rest = match latency.measured_ms {
        Some(measured) => format!("measured {measured:.1} ms in place of what the output says"),
        None => format!("output {:.1} ms", latency.output_ms),
    };
    let by_hand = if latency.adjust_ms != 0.0 {
        format!(" \u{00B7} by hand {:+.1} ms", latency.adjust_ms)
    } else {
        String::new()
    };
    format!(
        "Input {:.1} ms \u{00B7} hand-over {:.1} ms \u{00B7} {rest}{by_hand}",
        latency.input_ms, latency.handover_ms
    )
}

/// What a measurement found, as a sentence.
fn found(measured: &Measured) -> String {
    let against = if measured.more_than_said_ms >= 0.0 {
        format!("{:.1} ms more than the devices say", measured.more_than_said_ms)
    } else {
        format!("{:.1} ms less than the devices say", -measured.more_than_said_ms)
    };
    let sure = if measured.likeness >= 0.6 {
        "It came back clearly."
    } else {
        "It came back faintly: try again louder or closer before trusting it."
    };
    format!(
        "The sound took {:.1} ms to go round, {against}. {sure} (Heard at {}, {:.0} % alike.)",
        measured.round_trip_ms,
        units::format_level(measured.heard_db as f32),
        measured.likeness * 100.0
    )
}

/// A small action in the current row; returns whether it was pressed.
#[expect(clippy::too_many_arguments, reason = "a placement call: where, what it says, and whether it can be pressed")]
fn action(
    ui: &mut Ui,
    cx: &mut Cx,
    x: &mut f32,
    row: Rect,
    label: &str,
    name: &str,
    enabled: bool,
    hint: &str,
) -> bool {
    let theme = cx.theme;
    let chip = Chip::action(label).dimmed(!enabled).named(name);
    let width = chip.width(ui, theme);
    let rect = Rect::from_min_size(pos2(*x, row.center().y - theme.metric.chip / 2.0), vec2(width, theme.metric.chip));
    *x = rect.right() + GAP;
    let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with(("input-action", name)), &chip);
    cx.hint(&response, hint);
    response.clicked() && enabled
}

/// The input: what is open, what is arriving, and the round trip with its
/// measurement.
pub(super) fn section(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let id = ui.scope_id().with("input");
    heading(ui, theme, "Input");
    let quiet = TextStyle { size: 12.0, ..theme.text.body };

    // The level, and the switch that opens the input to show it.
    let (row, said) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let listening = cx.settings.input.listening;
    let chip = Chip::new("Show level", listening).accent(theme.signal).named("Show the input\u{2019}s level");
    let chip_rect = Rect::from_min_size(
        pos2(row.left(), row.center().y - theme.metric.chip / 2.0),
        vec2(chip.width(ui, theme), theme.metric.chip),
    );
    let response = trill_noir::chip(ui, theme, chip_rect, id.with("listen"), &chip);
    cx.hint(&response, "Open the input and show what is arriving on each channel. It closes with this panel");
    if response.clicked() {
        listen(cx, !listening);
    }
    let words = match (open_line(cx), cx.live.input.needed) {
        (Some(open), _) => open,
        (None, true) => "Opening the input\u{2026}".to_owned(),
        (None, false) => "The input is closed. It opens while a track that records it is armed or heard.".to_owned(),
    };
    let room = row.with_min_x(chip_rect.right() + 10.0);
    said.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Input: {words}")));
    let ink = if cx.live.input.open.is_some() { theme.ink } else { theme.ink_dim };
    text::paint(ui.painter(), room, Align2::LEFT_CENTER, quiet, &words, ink);
    if let Some(problem) = cx.live.input.problem.clone() {
        ui.add_space(4.0);
        paragraph(ui, theme, theme.warn, &super::sentence(&problem));
    }
    if cx.live.input.open.is_some() {
        ui.add_space(GAP);
        levels(ui, cx, id);
    }

    ui.add_space(12.0);
    heading(ui, theme, "Round trip");
    let Some(latency) = cx.live.input.latency else {
        let about = "How long sound takes from the microphone back to the speaker is known once the input is open. \
                     A take is put where it was sung by taking that off.";
        paragraph(ui, theme, theme.ink_dim, about);
        return;
    };
    let total = format!("Round trip: {}", inputs::round_trip_words(latency.round_trip_ms, latency.correction));
    let (line, response) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &total));
    text::paint(ui.painter(), line, Align2::LEFT_CENTER, theme.text.body, &total, theme.ink);
    let (line, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
    text::paint(ui.painter(), line, Align2::LEFT_CENTER, quiet, &figures(&latency), theme.ink_dim);
    if latency.correction == Correction::None {
        ui.add_space(2.0);
        let about =
            "These are the devices\u{2019} own figures, which are often short of the truth. Measure to be sure.";
        paragraph(ui, theme, theme.ink_dim, about);
    }
    ui.add_space(GAP);

    // The measurement: which channel listens, and the button.
    let channels = cx.live.input.open.as_ref().map_or(1, |open| open.channels).max(1);
    if cx.settings.input.channel == 0 || cx.settings.input.channel > channels {
        cx.settings.input.channel = 1;
    }
    let channel = cx.settings.input.channel;
    let measurement = cx.live.input.measurement.clone().unwrap_or(Measurement::Idle);
    let running = measurement == Measurement::Running;
    let recording = cx.live.take.is_some();
    let (row, _) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let mut x = row.left();
    let hint = "Plays a second of quiet noise and listens for it: connect the output to the input with a cable, or \
                hold the microphone to the speaker";
    let label = if running { "Listening\u{2026}" } else { "Measure" };
    if action(ui, cx, &mut x, row, label, "Measure the round trip", !running && !recording, hint) {
        cx.try_session(|session| session.measure_latency(channel));
    }
    let listen_rect = Rect::from_min_max(pos2(x, row.top()), pos2(x + 150.0, row.bottom()));
    let value = format!("Channel {channel}");
    let response = choice(ui, theme, listen_rect, id.with("measure-channel"), "On", &value, running);
    cx.hint(&response, "The channel of the input that the measurement listens on");
    if !running {
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(listen_rect.width());
            for choice in 1..=channels.min(16) {
                if MenuItem::new(&format!("Channel {choice}")).checked(choice == channel).show(ui, theme).clicked() {
                    cx.settings.input.channel = choice;
                }
            }
        });
    }
    x = listen_rect.right() + GAP;
    // The correction by hand: earlier, later, and none.
    let earlier = latency.adjust_ms + NUDGE_MS;
    let later = latency.adjust_ms - NUDGE_MS;
    let mut adjust = None;
    x += 8.0;
    if action(ui, cx, &mut x, row, "Earlier", "Put takes earlier", !recording, "Put takes half a millisecond earlier") {
        adjust = Some(earlier);
    }
    if action(ui, cx, &mut x, row, "Later", "Put takes later", !recording, "Put takes half a millisecond later") {
        adjust = Some(later);
    }
    // The two that undo are always there, and dim while there is nothing
    // to undo: a button that comes and goes moves the ones beside it.
    let nudged = latency.adjust_ms != 0.0 && !recording;
    if action(ui, cx, &mut x, row, "No nudge", "Take off the nudge", nudged, "Take the correction by hand off") {
        adjust = Some(0.0);
    }
    if let Some(milliseconds) = adjust {
        cx.sounds
            .quietly(move |runtime| runtime.keep_latency_adjustment(milliseconds).map_err(|error| error.to_string()));
    }
    let hint = "Go back to what the devices say of themselves";
    let measured = latency.measured_ms.is_some() && !recording;
    if action(ui, cx, &mut x, row, "Forget", "Forget the measurement", measured, hint) {
        cx.sounds.quietly(|runtime| runtime.keep_measured_latency(None).map_err(|error| error.to_string()));
    }

    // What the measurement found, and the offer.
    match measurement {
        Measurement::Idle | Measurement::Running => {}
        Measurement::Failed(why) => {
            ui.add_space(4.0);
            paragraph(ui, theme, theme.warn, &super::sentence(&why));
        }
        Measurement::Done(measured) => {
            ui.add_space(4.0);
            paragraph(ui, theme, theme.ink, &found(&measured));
            let taken = latency.measured_ms.is_some_and(|kept| (kept - measured.beyond_ms).abs() < 0.005);
            ui.add_space(4.0);
            let (row, _) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
            let mut x = row.left();
            if taken {
                let said = "In use: takes are put by what was measured, and it is remembered for these devices.";
                text::paint(ui.painter(), row, Align2::LEFT_CENTER, quiet, said, theme.ok);
            } else {
                let hint = "Put takes by what was measured, and remember it for these devices as they are open";
                if action(ui, cx, &mut x, row, "Use it", "Use what was measured", !recording, hint) {
                    let beyond = measured.beyond_ms;
                    cx.sounds.quietly(move |runtime| {
                        runtime.keep_measured_latency(Some(beyond)).map_err(|error| error.to_string())
                    });
                }
                let room = row.with_min_x(x + 4.0);
                let said = "Nothing changes until it is taken.";
                text::paint(ui.painter(), room, Align2::LEFT_CENTER, quiet, said, theme.ink_dim);
            }
        }
    }
}

/// A bar for each channel of the open input, with its lamp and its
/// highest level lately. A click puts the lamps out.
fn levels(ui: &mut Ui, cx: &mut Cx, id: Id) {
    let theme = cx.theme;
    let count = cx.live.input.channel_count().clamp(1, 16);
    let (area, response) = ui.allocate_exact_size(vec2(WIDTH, LEVELS), Sense::click());
    let painter = ui.painter_at(area);
    let _ = id;
    let rows = if count > 8 { 2 } else { 1 };
    let per_row = count.div_ceil(rows);
    let cell = (WIDTH + GAP) / per_row as f32;
    let height = (LEVELS - (rows as f32 - 1.0) * 4.0) / rows as f32;
    let mut said = Vec::new();
    for index in 0..count {
        let state = cx.live.input.channel(index);
        let (column, line) = (index % per_row, index / per_row);
        let rect = Rect::from_min_size(
            pos2(area.left() + column as f32 * cell, area.top() + line as f32 * (height + 4.0)),
            vec2(cell - GAP, height),
        );
        let label = Rect::from_min_max(rect.min, pos2(rect.left() + 14.0, rect.bottom()));
        text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.micro, &(index + 1).to_string(), theme.ink_faint);
        let lamp = Rect::from_min_max(
            pos2(rect.right() - 5.0, rect.center().y - 4.0),
            pos2(rect.right(), rect.center().y + 4.0),
        );
        painter.rect_filled(lamp, 0.0, if state.clipped { theme.signal } else { theme.well });
        let bar = Rect::from_min_max(
            pos2(label.right() + 2.0, rect.center().y - 3.0),
            pos2(lamp.left() - 3.0, rect.center().y + 3.0),
        );
        painter.rect_filled(bar, 0.0, theme.well);
        let level = widgets::levels(state);
        let zones = widgets::meter_zones();
        let x_at = |position: f32| bar.left() + bar.width() * position.clamp(0.0, 1.0);
        for (from, to, color) in
            [(0.0, zones.hot, theme.ok), (zones.hot, zones.over, theme.warn), (zones.over, 1.0, theme.signal)]
        {
            if level.left > from {
                let filled = Rect::from_x_y_ranges(x_at(from)..=x_at(level.left.min(to)), bar.y_range());
                painter.rect_filled(filled, 0.0, color);
            }
        }
        if let Some(peak) = level.peak {
            paint::vline(&painter, x_at(peak), bar.y_range(), theme.ink);
        }
        let clipped = if state.clipped { ", clipped" } else { "" };
        said.push(format!("{} at {}{clipped}", index + 1, units::format_level(state.peak_db)));
    }
    let name = format!("Input level: {}", said.join(" \u{00B7} "));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    cx.hint(&response, "What is arriving on each channel; a lamp stays lit once a channel was at full scale \u{00B7} click to put the lamps out");
    if response.clicked() {
        cx.state.clear_input_lamps = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, is_default: bool, channels: u16) -> AudioDevice {
        AudioDevice {
            name: name.into(),
            is_default,
            default_sample_rate: 48_000,
            sample_rates: vec![48_000],
            channels,
            exclusive: true,
        }
    }

    #[test]
    fn the_input_named_is_the_one_shown_and_the_default_where_none_is() {
        let devices =
            AudioDevices { outputs: Vec::new(), inputs: vec![device("Line", false, 2), device("Microphone", true, 1)] };
        let mut draft = AudioConfig::default();
        assert_eq!(device_label(&draft, Some(&devices)), "System default \u{00B7} Microphone");
        assert_eq!(device_label(&draft, None), "System default");
        assert_eq!(chosen(&draft, &devices).map(|device| device.channels), Some(1));
        draft.input = Some("Line".into());
        assert_eq!(device_label(&draft, Some(&devices)), "Line");
        assert_eq!(chosen(&draft, &devices).map(|device| device.channels), Some(2));
        draft.input = Some("Gone".into());
        assert_eq!(device_label(&draft, Some(&devices)), "Gone \u{00B7} not connected");
        assert_eq!(device_label(&AudioConfig::default(), Some(&AudioDevices::default())), "None connected");
    }

    #[test]
    fn a_round_trip_says_what_it_is_made_of_and_a_measurement_what_it_found() {
        let said = RecordingLatency {
            output_ms: 30.0,
            input_ms: 5.0,
            handover_ms: 25.5,
            measured_ms: None,
            adjust_ms: 0.0,
            correction: Correction::None,
            round_trip_ms: 60.5,
            drift_ppm: 0.0,
        };
        assert_eq!(figures(&said), "Input 5.0 ms \u{00B7} hand-over 25.5 ms \u{00B7} output 30.0 ms");
        let measured = RecordingLatency { measured_ms: Some(33.4), adjust_ms: -1.5, ..said };
        assert_eq!(
            figures(&measured),
            "Input 5.0 ms \u{00B7} hand-over 25.5 ms \u{00B7} measured 33.4 ms in place of what the output says \u{00B7} by \
             hand -1.5 ms"
        );
        let clear =
            Measured { round_trip_ms: 58.9, beyond_ms: 33.4, more_than_said_ms: 8.4, likeness: 0.98, heard_db: -21.0 };
        let words = found(&clear);
        assert!(words.starts_with("The sound took 58.9 ms to go round, 8.4 ms more than the devices say."), "{words}");
        assert!(words.contains("It came back clearly."), "{words}");
        let faint = Measured { more_than_said_ms: -2.0, likeness: 0.2, ..clear };
        let words = found(&faint);
        assert!(words.contains("2.0 ms less than the devices say") && words.contains("faintly"), "{words}");
        // Two clocks: how far apart they run, as it is read.
        assert_eq!(drift_words(12.34), "12.3 parts in a million fast");
        assert_eq!(drift_words(-3.0), "3.0 parts in a million slow");
        assert_eq!(drift_words(0.01), "in step with the output\u{2019}s");
    }
}
