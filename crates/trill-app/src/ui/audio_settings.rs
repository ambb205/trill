//! The Audio and MIDI panel: which device plays, at what rate and buffer,
//! what is really running and how late it is heard, and which MIDI inputs
//! are listened to.
//!
//! Three rules shape it.
//!
//! **Nothing here waits on hardware.** Listing the audio devices, opening a
//! stream, switching a MIDI port and looking for new ones all ask a driver,
//! which answers in a moment or, on a bad day, in seconds. Each runs on a
//! worker ([`Sounds::quietly`](super::sounds::Sounds::quietly)) and the
//! panel says what it is waiting for. While a stream is being opened the
//! session is busy, so the panel reads what is running from the copy the
//! window keeps ([`AudioSummary`]) and never from the session itself.
//!
//! **A choice is seen before it is made.** The form edits a draft of what
//! is asked of the hardware; nothing changes until Apply. Under the form
//! stands what applying it will cost: exclusive mode silences other
//! programs on the device, a change of sample rate stops playback and
//! closes plugin windows.
//!
//! **Whatever happens reads as a sentence.** A device that is gone, a list
//! that comes back empty, a stream that had to fall back, a port another
//! program holds: each is said in the panel, in the engine's own words
//! where it has some, and nothing is thrown at the user as an error.

mod input;

pub use input::input_device;

use super::state::{AudioSummary, Cx};
use super::status_bar::format_rate;
use eframe::egui::{self, Align, Align2, Id, Layout, Rect, Response, Sense, Ui, UiBuilder, pos2, vec2};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use trill_engine::host::{COMMON_BUFFER_SIZES, COMMON_SAMPLE_RATES};
use trill_engine::{AudioConfig, AudioDevice, AudioDevices};
use trill_noir::{Chip, Icon, MenuItem, TextStyle, Theme, Variant, fade, paint, prose, text};
use trill_runtime::MidiInputInfo;

const WIDTH: f32 = 580.0;
/// Height of a row of the form, and of a MIDI input.
const ROW: f32 = 22.0;
const GAP: f32 = 6.0;
/// How long a MIDI input's lamp stays lit after a message, in seconds.
const LAMP_SECONDS: f64 = 0.18;
/// How often the panel looks at the lamps while it is open, in seconds.
const LAMP_REFRESH: f64 = 0.06;
/// More MIDI inputs than this scroll.
const MAX_MIDI_ROWS: usize = 6;

/// What a worker reports back.
enum Done {
    /// The audio devices, as the system lists them now.
    Devices(AudioDevices),
    /// The stream was opened as asked, or as near as could be. Carries
    /// what went wrong writing the choice down, if something did.
    Applied(Result<(), String>),
    /// A MIDI input was switched; the same for its place in the settings.
    MidiSwitched(Result<(), String>),
    /// The MIDI ports were looked at, or the system did not answer in time.
    Rescanned(bool),
}

/// The pages of the panel.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum Page {
    /// The devices, what is running, and the MIDI inputs.
    #[default]
    Devices,
    /// What is arriving on the input, and how long sound takes to go round.
    Input,
}

/// The panel and what it is waiting for.
pub struct Settings {
    open: bool,
    page: Page,
    /// What the form shows: the wish being edited.
    draft: AudioConfig,
    /// The audio devices as last listed; `None` until a list has come.
    devices: Option<AudioDevices>,
    listing: bool,
    applying: bool,
    rescanning: bool,
    /// Why the choice in use could not be written down, if it could not.
    save_problem: Option<String>,
    /// Set when the system did not answer a look at the MIDI ports.
    rescan_timed_out: bool,
    /// The MIDI inputs as last read.
    midi: Vec<MidiInputInfo>,
    /// Per input: the message count last seen, and when it last moved.
    lamps: HashMap<String, (u64, f64)>,
    /// The audio input's part: whether its level is on show, and which
    /// channel a measurement listens on.
    input: input::InputPanel,
    sender: Sender<Done>,
    receiver: Receiver<Done>,
}

impl Default for Settings {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            open: false,
            page: Page::default(),
            draft: AudioConfig::default(),
            devices: None,
            listing: false,
            applying: false,
            rescanning: false,
            save_problem: None,
            rescan_timed_out: false,
            midi: Vec::new(),
            lamps: HashMap::new(),
            input: input::InputPanel::default(),
            sender,
            receiver,
        }
    }
}

impl Settings {
    /// True while the panel is on screen: the keyboard is then its own.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// True while a stream is being opened: the session is busy, and
    /// nothing on the window's own thread may wait for it.
    pub fn is_applying(&self) -> bool {
        self.applying
    }

    /// True while the panel shows the input's level, and so holds the
    /// input open.
    pub fn is_listening(&self) -> bool {
        self.input.is_listening()
    }

    /// True while the panel shows something that is still on its way: the
    /// list of devices, a stream being opened, a look at the MIDI ports. A
    /// screenshot waits for it.
    pub fn is_waiting(&self) -> bool {
        self.open && (self.listing || self.applying || self.rescanning)
    }

    /// Takes in what the workers have finished.
    pub fn poll(&mut self) {
        while let Ok(done) = self.receiver.try_recv() {
            match done {
                Done::Devices(devices) => {
                    self.devices = Some(devices);
                    self.listing = false;
                }
                Done::Applied(saved) => {
                    self.applying = false;
                    self.save_problem = saved.err();
                }
                Done::MidiSwitched(saved) => {
                    if let Err(problem) = saved {
                        self.save_problem = Some(problem);
                    }
                }
                Done::Rescanned(finished) => {
                    self.rescanning = false;
                    self.rescan_timed_out = !finished;
                }
            }
        }
    }

    #[cfg(test)]
    pub fn draft(&self) -> &AudioConfig {
        &self.draft
    }

    /// Whether the lamp of the MIDI input called `name` is lit at `now`.
    #[cfg(test)]
    pub fn lamp_is_lit(&self, name: &str, now: f64) -> bool {
        self.lamps.get(name).is_some_and(|(_, at)| now - at < LAMP_SECONDS)
    }

    /// Reports, as a worker would, that the system did not answer a look
    /// at the MIDI ports in time: no scripted port can be that slow.
    #[cfg(test)]
    pub fn report_rescan_timed_out(&self) {
        let _ = self.sender.send(Done::Rescanned(false));
    }
}

/// Opens the panel on what is asked of the hardware now, and starts
/// looking for the devices there are.
pub fn open(cx: &mut Cx) {
    cx.settings.open = true;
    cx.settings.draft = cx.audio.config.clone();
    cx.settings.save_problem = None;
    cx.settings.rescan_timed_out = false;
    list_devices(cx);
}

pub fn toggle(cx: &mut Cx) {
    if cx.settings.open { close(cx) } else { open(cx) }
}

/// Opens the panel on its page of the input.
pub fn open_on_input(cx: &mut Cx) {
    open(cx);
    cx.settings.page = Page::Input;
}

/// Opens the panel with the input's level on show, which opens the input.
pub fn open_showing_level(cx: &mut Cx) {
    open_on_input(cx);
    input::listen(cx, true);
}

/// Puts the panel away, unless a stream is being opened: the session is
/// busy for as long as that takes, and with the panel gone the next click
/// in the window would wait for it. The panel stays up and says what it
/// is waiting for; a window that waits with nothing to show for it is a
/// window that has frozen.
pub fn close(cx: &mut Cx) {
    if !cx.settings.applying {
        // The panel no longer shows the input's level, so it no longer
        // holds the input open.
        input::closed(cx);
        cx.settings.open = false;
    }
}

fn list_devices(cx: &mut Cx) {
    if std::mem::replace(&mut cx.settings.listing, true) {
        return;
    }
    let sender = cx.settings.sender.clone();
    cx.sounds.quietly(move |runtime| {
        // The receiver is gone only when the window has closed.
        let _ = sender.send(Done::Devices(runtime.audio_devices()));
        Ok(())
    });
}

fn apply(cx: &mut Cx) {
    if std::mem::replace(&mut cx.settings.applying, true) {
        return;
    }
    cx.settings.save_problem = None;
    let (sender, config) = (cx.settings.sender.clone(), cx.settings.draft.clone());
    cx.sounds.quietly(move |runtime| {
        let (_, saved) = runtime.set_audio(config);
        let _ = sender.send(Done::Applied(saved.map_err(|error| error.to_string())));
        // What can be chosen may have changed with it.
        let _ = sender.send(Done::Devices(runtime.audio_devices()));
        Ok(())
    });
}

fn switch_midi(cx: &mut Cx, name: String, on: bool) {
    let sender = cx.settings.sender.clone();
    cx.sounds.quietly(move |runtime| {
        let saved = runtime.set_midi_input_enabled(&name, on).map_err(|error| error.to_string());
        let _ = sender.send(Done::MidiSwitched(saved));
        // The port is opened or closed at the next look; look now.
        let _ = sender.send(Done::Rescanned(runtime.rescan_midi_inputs()));
        Ok(())
    });
}

fn rescan_midi(cx: &mut Cx) {
    if std::mem::replace(&mut cx.settings.rescanning, true) {
        return;
    }
    cx.settings.rescan_timed_out = false;
    let sender = cx.settings.sender.clone();
    cx.sounds.quietly(move |runtime| {
        let _ = sender.send(Done::Rescanned(runtime.rescan_midi_inputs()));
        Ok(())
    });
}

// ---- what the form says, as pure functions -----------------------------------

/// The device the draft names among `devices`, or the system's default
/// when it names none.
fn chosen<'a>(draft: &AudioConfig, devices: &'a AudioDevices) -> Option<&'a AudioDevice> {
    match &draft.output {
        Some(name) => devices.outputs.iter().find(|device| device.name == *name),
        None => devices.outputs.iter().find(|device| device.is_default).or(devices.outputs.first()),
    }
}

/// How the draft's device reads in the form.
fn device_label(draft: &AudioConfig, devices: Option<&AudioDevices>) -> String {
    match (&draft.output, devices) {
        (None, Some(devices)) => match chosen(draft, devices) {
            Some(device) => format!("System default \u{00B7} {}", device.name),
            None => "System default".to_owned(),
        },
        (None, None) => "System default".to_owned(),
        (Some(name), Some(devices)) if !devices.outputs.iter().any(|device| device.name == *name) => {
            format!("{name} \u{00B7} not connected")
        }
        (Some(name), _) => name.clone(),
    }
}

/// `frames` at `rate` in words: `"256 frames · 5.3 ms"`.
fn buffer_label(frames: u32, rate: f64) -> String {
    if rate > 0.0 {
        format!("{frames} frames \u{00B7} {:.1} ms", frames as f64 * 1000.0 / rate)
    } else {
        format!("{frames} frames")
    }
}

/// The sample rates the draft's device can be asked for.
fn rates(draft: &AudioConfig, devices: Option<&AudioDevices>) -> Vec<u32> {
    match devices.and_then(|devices| chosen(draft, devices)) {
        Some(device) if !device.sample_rates.is_empty() => device.sample_rates.clone(),
        _ => COMMON_SAMPLE_RATES.to_vec(),
    }
}

/// The rate the draft would run at, as far as can be told: the one it asks
/// for, or its device's own.
fn draft_rate(draft: &AudioConfig, devices: Option<&AudioDevices>) -> Option<u32> {
    draft.sample_rate.or_else(|| devices.and_then(|devices| chosen(draft, devices)).map(|d| d.default_sample_rate))
}

/// What applying `draft` will cost, each as a sentence, given what runs
/// now. Empty when it costs nothing worth saying, or changes nothing.
fn costs(draft: &AudioConfig, audio: &AudioSummary, devices: Option<&AudioDevices>, recording: bool) -> Vec<String> {
    let mut costs = Vec::new();
    if *draft == audio.config {
        return costs;
    }
    let device = devices.and_then(|devices| chosen(draft, devices));
    let name = device.map_or_else(|| "the device".to_owned(), |device| device.name.clone());
    if draft.exclusive && device.is_some_and(|device| !device.exclusive) {
        costs.push(format!("{name} cannot be taken for one program alone. It will be opened shared."));
    } else if draft.exclusive && !(audio.config.exclusive && draft.output == audio.config.output) {
        costs.push(format!(
            "Exclusive mode takes {name} for Trill alone: other programs make no sound on it while Trill runs."
        ));
    }
    match draft_rate(draft, devices) {
        Some(rate) if rate as f64 != audio.info.sample_rate => costs.push(format!(
            "Going from {} to {} stops playback and closes any open plugin windows.",
            format_rate(audio.info.sample_rate),
            format_rate(rate as f64)
        )),
        None if draft.output != audio.config.output => costs.push(
            "If that device runs at another sample rate, playback stops and open plugin windows close.".to_owned(),
        ),
        _ => {}
    }
    if draft.buffer_size.is_some() && !draft.exclusive {
        costs.push(
            "A shared device runs at the system\u{2019}s own buffer size. The size chosen here is honored in \
             exclusive mode."
                .to_owned(),
        );
    }
    if recording {
        costs.push("The take being recorded ends when the device changes.".to_owned());
    }
    if costs.is_empty() {
        costs.push("The sound stops for a moment while the device is opened.".to_owned());
    }
    costs
}

/// What is running, on one line: rate, buffer, sharing and latency.
fn running_line(audio: &AudioSummary) -> String {
    let info = &audio.info;
    let sharing = if info.exclusive { "exclusive" } else { "shared" };
    format!(
        "{} \u{00B7} {} \u{00B7} {sharing} \u{00B7} heard {:.1} ms later, as the device reports",
        format_rate(info.sample_rate),
        buffer_label(info.buffer_frames, info.sample_rate),
        audio.latency_ms
    )
}

/// Why the stream is not the one asked for, in the engine's words, and
/// what happens next: a device that is asked for by name is gone back to
/// when it returns, since the engine looks for it as long as it is wanted.
fn fallen_back(audio: &AudioSummary, warning: &str) -> String {
    let mut words = sentence(warning);
    if let Some(wanted) = audio.config.output.as_ref().filter(|wanted| audio.info.device.as_ref() != Some(*wanted)) {
        words.push_str(&format!(" Trill goes back to {wanted} when it is connected again."));
    }
    words
}

/// A sentence as the engine words it, with a capital and a full stop.
fn sentence(words: &str) -> String {
    let words = words.trim();
    let mut letters = words.chars();
    let Some(first) = letters.next() else { return String::new() };
    let mut sentence: String = first.to_uppercase().chain(letters).collect();
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

/// What a MIDI input is doing, and whether that is a problem.
fn midi_state(input: &MidiInputInfo) -> (String, bool) {
    match (&input.problem, input.enabled, input.present, input.open) {
        (_, false, false, _) => ("Off \u{00B7} not plugged in".to_owned(), false),
        (_, false, ..) => ("Off".to_owned(), false),
        (Some(problem), ..) => (sentence(&format!("Not listening: {problem}")), true),
        (None, true, false, _) => ("Not plugged in".to_owned(), false),
        (None, true, true, true) => ("Listening".to_owned(), false),
        (None, true, true, false) => ("Opening\u{2026}".to_owned(), false),
    }
}

// ---- the panel -----------------------------------------------------------------

/// A caps heading over a rule, with room at its right end for a button.
fn heading(ui: &mut Ui, theme: &Theme, title: &str) -> Rect {
    let (row, _) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
    text::paint(ui.painter(), row, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
    paint::hline_above(ui.painter(), row.x_range(), row.bottom(), theme.hairline);
    ui.add_space(GAP);
    row
}

/// A paragraph that wraps at the panel's width.
fn paragraph(ui: &mut Ui, theme: &Theme, color: egui::Color32, words: &str) {
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let galley = prose::plain(ui, theme, style, color, words, WIDTH);
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, galley.size().y), Sense::hover());
    // The words are painted; this is them for whoever cannot see paint.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, words));
    prose::paint(ui, rect.min, galley);
}

/// A row of the form: a label, what is chosen, and the caret of something
/// that opens. The choice is set as it is spelled, since a device's name
/// is its own; the kit's dropdown would set it in capitals.
fn choice(ui: &Ui, theme: &Theme, rect: Rect, id: Id, label: &str, value: &str, dimmed: bool) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let name = format!("{label}: {value}");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, !dimmed, &name));
    let painter = ui.painter_at(rect);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    if open {
        painter.rect_filled(rect, 0.0, theme.selected);
    } else if response.hovered() && !dimmed {
        painter.rect_filled(rect, 0.0, theme.hover);
    }
    paint::frame(&painter, rect, theme.hairline);
    let content = Rect::from_min_max(pos2(rect.left() + 8.0, rect.top()), pos2(rect.right() - 20.0, rect.bottom()));
    let used = text::paint(&painter, content, Align2::LEFT_CENTER, theme.text.label, label, theme.ink_faint);
    let ink = if dimmed { fade(theme.ink, theme.disabled) } else { theme.ink };
    let style = TextStyle { size: 12.0, ..theme.text.body };
    text::paint(&painter, content.with_min_x(content.left() + used + 14.0), Align2::RIGHT_CENTER, style, value, ink);
    let caret = if dimmed { fade(theme.ink_dim, theme.disabled) } else { theme.ink_dim };
    trill_noir::paint_icon(&painter, pos2(rect.right() - 10.0, rect.center().y), Icon::CaretDown, caret);
    response
}

/// A small button at the right end of a heading.
fn heading_button(ui: &mut Ui, cx: &mut Cx, row: Rect, label: &str, busy: bool, hint: &str) -> bool {
    let theme = cx.theme;
    let chip = Chip::action(label).dimmed(busy);
    let width = chip.width(ui, theme);
    let rect = Rect::from_min_size(pos2(row.right() - width, row.top()), vec2(width, theme.metric.chip));
    let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with(("heading", label)), &chip);
    cx.hint(&response, hint);
    response.clicked() && !busy
}

/// Shows the panel while it is open.
pub fn modal(ctx: &egui::Context, cx: &mut Cx) {
    if !cx.settings.open {
        return;
    }
    let theme = cx.theme;
    // The lamps are read while the panel is up; cheap, and asks no driver.
    let now = ctx.input(|input| input.time);
    cx.settings.midi = cx.sounds.runtime.midi_inputs();
    for input in &cx.settings.midi {
        let lamp = cx.settings.lamps.entry(input.name.clone()).or_insert((input.events, f64::NEG_INFINITY));
        if lamp.0 != input.events {
            *lamp = (input.events, now);
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(LAMP_REFRESH));

    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let modal =
        egui::Modal::new(Id::unique("audio-settings")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Audio and MIDI", theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);
            ui.add_space(8.0);
            let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(8.0);

            // Two pages, so that the whole of either stands in the
            // smallest window: what plays and what is played with, and
            // what is recorded.
            let (tabs, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.chip), Sense::hover());
            let labels = ["Devices and MIDI", "Input and round trip"];
            let shown = cx.settings.page as usize;
            let (clicked, _) = trill_noir::chip_row(
                ui,
                theme,
                tabs.min,
                WIDTH,
                ui.scope_id().with("pages"),
                &labels,
                Some(shown),
                false,
            );
            match clicked {
                Some(0) => cx.settings.page = Page::Devices,
                Some(_) => cx.settings.page = Page::Input,
                None => {}
            }
            ui.add_space(10.0);
            match cx.settings.page {
                Page::Devices => {
                    output(ui, cx);
                    ui.add_space(14.0);
                    running(ui, cx);
                    ui.add_space(14.0);
                    midi(ui, cx, now);
                }
                Page::Input => input::section(ui, cx),
            }
        });
    if modal.should_close() {
        close(cx);
    }
}

/// The form: the device that plays and the one recorded from, sample rate,
/// buffer and exclusive mode, what applying it costs, and the buttons that
/// apply it or put it back.
fn output(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let id = ui.scope_id().with("output");
    let busy = cx.settings.applying;
    let row = heading(ui, theme, "Devices");
    let listing = cx.settings.listing;
    if heading_button(ui, cx, row, "Refresh", listing, "Look again for the audio devices that are connected") {
        list_devices(cx);
    }

    let devices = cx.settings.devices.clone();
    let mut draft = cx.settings.draft.clone();

    // The device.
    let (line, _) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let label = device_label(&draft, devices.as_ref());
    let response = choice(ui, theme, line, id.with("device"), "Device", &label, busy);
    cx.hint(&response, "The device Trill plays through");
    if !busy {
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(line.width());
            let default = AudioConfig { output: None, ..draft.clone() };
            let named = device_label(&default, devices.as_ref());
            if MenuItem::new(&named).checked(draft.output.is_none()).show(ui, theme).clicked() {
                draft.output = None;
            }
            let outputs = devices.as_ref().map(|devices| devices.outputs.as_slice()).unwrap_or_default();
            if !outputs.is_empty() {
                trill_noir::menu_separator(ui, theme);
            }
            for device in outputs {
                let current = draft.output.as_deref() == Some(device.name.as_str());
                if MenuItem::new(&device.name).checked(current).show(ui, theme).clicked() {
                    draft.output = Some(device.name.clone());
                }
            }
        });
    }
    ui.add_space(theme.metric.gap);

    // The device that is recorded from.
    input::device_row(ui, cx, &mut draft, devices.as_ref(), id);
    ui.add_space(theme.metric.gap);

    // Sample rate and buffer, side by side.
    let (line, _) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let half = ((WIDTH - GAP) / 2.0).floor();
    let rate_rect = Rect::from_min_size(line.min, vec2(half, ROW));
    let buffer_rect = Rect::from_min_max(pos2(rate_rect.right() + GAP, line.top()), line.max);
    let offered = rates(&draft, devices.as_ref());
    let own_rate = "The device\u{2019}s own";
    let rate_label = draft.sample_rate.map_or_else(|| own_rate.to_owned(), |rate| format_rate(rate as f64));
    let response = choice(ui, theme, rate_rect, id.with("rate"), "Sample rate", &rate_label, busy);
    cx.hint(&response, "How many samples a second the device runs at");
    if !busy {
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(rate_rect.width());
            if MenuItem::new(own_rate).checked(draft.sample_rate.is_none()).show(ui, theme).clicked() {
                draft.sample_rate = None;
            }
            trill_noir::menu_separator(ui, theme);
            for rate in &offered {
                let current = draft.sample_rate == Some(*rate);
                if MenuItem::new(&format_rate(*rate as f64)).checked(current).show(ui, theme).clicked() {
                    draft.sample_rate = Some(*rate);
                }
            }
        });
    }
    let rate = draft_rate(&draft, devices.as_ref()).map_or(cx.audio.info.sample_rate, |rate| rate as f64);
    let own_buffer = "The device\u{2019}s own";
    let buffer_text = draft.buffer_size.map_or_else(|| own_buffer.to_owned(), |frames| buffer_label(frames, rate));
    let response = choice(ui, theme, buffer_rect, id.with("buffer"), "Buffer", &buffer_text, busy);
    cx.hint(
        &response,
        "How much audio the device asks for at a time: smaller is sooner heard, and harder to keep up with",
    );
    if !busy {
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(buffer_rect.width());
            if MenuItem::new(own_buffer).checked(draft.buffer_size.is_none()).show(ui, theme).clicked() {
                draft.buffer_size = None;
            }
            trill_noir::menu_separator(ui, theme);
            for frames in COMMON_BUFFER_SIZES {
                let current = draft.buffer_size == Some(frames);
                if MenuItem::new(&buffer_label(frames, rate)).checked(current).show(ui, theme).clicked() {
                    draft.buffer_size = Some(frames);
                }
            }
        });
    }
    ui.add_space(theme.metric.gap);

    // Exclusive mode, and what it is.
    let (line, _) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let chip = Chip::new("Exclusive", draft.exclusive).dimmed(busy).named("Exclusive mode");
    let chip_rect = Rect::from_min_size(
        pos2(line.left(), line.center().y - theme.metric.chip / 2.0),
        vec2(chip.width(ui, theme), theme.metric.chip),
    );
    let response = trill_noir::chip(ui, theme, chip_rect, id.with("exclusive"), &chip);
    cx.hint(&response, "Take the device for Trill alone, for the shortest latency");
    if response.clicked() && !busy {
        draft.exclusive = !draft.exclusive;
    }
    let about = "Takes the device for Trill alone: the buffer is then as short as asked.";
    let room = line.with_min_x(chip_rect.right() + 10.0);
    let quiet = TextStyle { size: 12.0, ..theme.text.body };
    text::paint(ui.painter(), room, Align2::LEFT_CENTER, quiet, about, theme.ink_dim);

    // A device that does not offer the rate asked for is asked for its own.
    if draft.output != cx.settings.draft.output
        && draft.sample_rate.is_some_and(|rate| !rates(&draft, devices.as_ref()).contains(&rate))
    {
        draft.sample_rate = None;
    }
    cx.settings.draft = draft.clone();

    // What the list could not say, and what the choice will cost.
    ui.add_space(8.0);
    let found = devices.as_ref().map(|devices| devices.outputs.len());
    if cx.settings.listing && found.is_none() {
        paragraph(ui, theme, theme.ink_dim, "Looking for the audio devices\u{2026}");
    } else if found == Some(0) {
        let words = "No audio output is connected. If one has just been plugged in, press Refresh in a moment.";
        paragraph(ui, theme, theme.warn, words);
    }
    let recording = cx.live.take.is_some();
    let changed = draft != cx.audio.config;
    for cost in costs(&draft, cx.audio, devices.as_ref(), recording) {
        paragraph(ui, theme, theme.ink, &cost);
        ui.add_space(2.0);
    }
    if let Some(problem) = cx.settings.save_problem.clone() {
        let words = sentence(&format!("The choice is in use, but could not be written down for next time: {problem}"));
        paragraph(ui, theme, theme.warn, &words);
    }

    // Apply, and Revert.
    ui.add_space(8.0);
    let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    let note = match (busy, changed) {
        (true, _) => "Opening the device\u{2026}",
        (false, true) => "Not applied yet",
        (false, false) => "This is what Trill asks for now",
    };
    text::paint(ui.painter(), foot, Align2::LEFT_CENTER, theme.text.micro, note, theme.ink_faint);
    let layout = Layout::right_to_left(Align::Center);
    let mut cell = ui.new_child(UiBuilder::new().id_salt("apply").max_rect(foot).layout(layout));
    cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
    if busy || !changed {
        cell.disable();
    }
    if trill_noir::button(&mut cell, theme, "Apply", Variant::Primary).clicked() {
        apply(cx);
    }
    if trill_noir::button(&mut cell, theme, "Revert", Variant::Default).clicked() {
        cx.settings.draft = cx.audio.config.clone();
    }
}

/// What is running, and why it is not what was asked for when it is not.
fn running(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    heading(ui, theme, "Running now");
    let info = &cx.audio.info;
    let (line, response) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
    let name = info.device.clone().unwrap_or_else(|| "No audio device".to_owned());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Running now: {name}")));
    text::paint(ui.painter(), line, Align2::LEFT_CENTER, theme.text.body, &name, theme.ink);
    // A stream that is not what was asked for says why in the engine's
    // words; a run that was asked to go without sound says that.
    if info.device.is_some() || info.warning.is_none() {
        let (line, response) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
        let words = if info.device.is_some() {
            running_line(cx.audio)
        } else {
            format!("Trill is running without sound, at {}", format_rate(info.sample_rate))
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &words));
        let style = TextStyle { size: 12.0, ..theme.text.body };
        text::paint(ui.painter(), line, Align2::LEFT_CENTER, style, &words, theme.ink_dim);
    }
    if let Some(warning) = &info.warning {
        ui.add_space(4.0);
        paragraph(ui, theme, theme.warn, &fallen_back(cx.audio, warning));
    }
}

/// The MIDI inputs: a lamp, a name, what the input is doing, and its switch.
fn midi(ui: &mut Ui, cx: &mut Cx, now: f64) {
    let theme = cx.theme;
    let row = heading(ui, theme, "MIDI inputs");
    let rescanning = cx.settings.rescanning;
    if heading_button(ui, cx, row, "Rescan", rescanning, "Look again for the MIDI devices that are plugged in") {
        rescan_midi(cx);
    }
    let inputs = cx.settings.midi.clone();
    if inputs.is_empty() {
        let keys = super::shortcuts::keys(super::shortcuts::Command::ToggleKeyboard);
        let words = format!(
            "No MIDI input is plugged in. Plug a keyboard in and press Rescan; it is listened to without further \
             ado. Until then the computer keyboard plays ({keys})."
        );
        paragraph(ui, theme, theme.ink_dim, &words);
    }
    let shown = inputs.len().min(MAX_MIDI_ROWS) as f32;
    let mut switched = None;
    egui::ScrollArea::vertical().max_height(shown * (ROW + 2.0)).auto_shrink([false, true]).show(ui, |ui| {
        for (index, input) in inputs.iter().enumerate() {
            let (line, response) = ui.allocate_exact_size(vec2(WIDTH, ROW + 2.0), Sense::hover());
            let (state, problem) = midi_state(input);
            let spoken = format!("{} \u{00B7} {state}", input.name);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &spoken));
            if index > 0 {
                paint::hline(ui.painter(), line.x_range(), line.top(), theme.grid);
            }
            // The lamp: lit for a moment after every message.
            let lit = cx.settings.lamps.get(&input.name).is_some_and(|(_, at)| now - at < LAMP_SECONDS);
            let lamp = pos2(line.left() + 2.0, line.center().y);
            let after = match (lit, input.open) {
                (true, _) => paint::dot(ui.painter(), lamp, theme.metric.dot, theme.ok),
                (false, true) => paint::dot(ui.painter(), lamp, theme.metric.dot, theme.ghost),
                (false, false) => paint::hollow_dot(ui.painter(), lamp, theme.metric.dot, theme.ghost),
            };
            // The switch at the right; the name and the state share the rest.
            let chip = Chip::new(if input.enabled { "On" } else { "Off" }, input.enabled);
            let name = format!("Listen to {}", input.name);
            let chip = chip.named(&name);
            let width = Chip::new("Off", false).width(ui, theme).max(Chip::new("On", false).width(ui, theme));
            let chip_rect = Rect::from_min_size(
                pos2(line.right() - width, line.center().y - theme.metric.chip / 2.0),
                vec2(width, theme.metric.chip),
            );
            let response = trill_noir::chip(ui, theme, chip_rect, ui.scope_id().with(("midi", index)), &chip);
            cx.hint(&response, "Whether what this input sends is played");
            if response.clicked() {
                switched = Some((input.name.clone(), !input.enabled));
            }
            let words = Rect::from_min_max(pos2(after + 8.0, line.top()), pos2(chip_rect.left() - 12.0, line.bottom()));
            let ink = if input.enabled && input.present { theme.ink } else { theme.ink_dim };
            let used = text::paint(ui.painter(), words, Align2::LEFT_CENTER, theme.text.body, &input.name, ink);
            let rest = words.with_min_x(words.left() + used + 14.0);
            let style = TextStyle { size: 12.0, ..theme.text.body };
            let color = if problem { theme.warn } else { theme.ink_dim };
            text::paint(ui.painter(), rest, Align2::LEFT_CENTER, style, &state, color);
        }
    });
    if let Some((name, on)) = switched {
        switch_midi(cx, name, on);
    }
    if cx.settings.rescan_timed_out {
        ui.add_space(4.0);
        let words = "The system did not answer within a few seconds. The list is as it was; try Rescan again.";
        paragraph(ui, theme, theme.warn, words);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_engine::StreamInfo;

    fn device(name: &str, default: bool, exclusive: bool) -> AudioDevice {
        AudioDevice {
            name: name.into(),
            is_default: default,
            default_sample_rate: 48_000,
            sample_rates: vec![44_100, 48_000],
            channels: 2,
            exclusive,
        }
    }

    fn devices() -> AudioDevices {
        AudioDevices {
            outputs: vec![
                device("Speakers", true, true),
                device("Interface", false, true),
                device("HDMI", false, false),
            ],
            inputs: Vec::new(),
        }
    }

    fn running(config: AudioConfig) -> AudioSummary {
        AudioSummary {
            config,
            info: StreamInfo {
                sample_rate: 48_000.0,
                output: "Speakers (48000 Hz)".into(),
                warning: None,
                device: Some("Speakers".into()),
                buffer_frames: 480,
                exclusive: false,
                latency_frames: 1440,
            },
            latency_ms: 31.9,
            revision: 0,
        }
    }

    #[test]
    fn the_form_names_the_device_it_means() {
        let devices = devices();
        let default = AudioConfig::default();
        assert_eq!(device_label(&default, Some(&devices)), "System default \u{00B7} Speakers");
        assert_eq!(device_label(&default, None), "System default", "before the list has come");
        let interface = AudioConfig { output: Some("Interface".into()), ..default.clone() };
        assert_eq!(device_label(&interface, Some(&devices)), "Interface");
        let gone = AudioConfig { output: Some("Old Interface".into()), ..default.clone() };
        assert_eq!(device_label(&gone, Some(&devices)), "Old Interface \u{00B7} not connected");
        assert_eq!(device_label(&gone, None), "Old Interface", "nothing is said of a device before it is looked for");
        assert_eq!(device_label(&default, Some(&AudioDevices::default())), "System default");
        assert_eq!(chosen(&gone, &devices), None);
        assert_eq!(chosen(&default, &devices).unwrap().name, "Speakers");
    }

    #[test]
    fn a_choice_says_what_it_costs_before_it_is_made() {
        let devices = devices();
        let now = running(AudioConfig::default());
        let said = |draft: &AudioConfig, recording: bool| costs(draft, &now, Some(&devices), recording);
        assert!(said(&AudioConfig::default(), false).is_empty(), "nothing changes, nothing is said");

        let exclusive = AudioConfig { exclusive: true, ..AudioConfig::default() };
        let cost = said(&exclusive, false);
        assert_eq!(cost.len(), 1);
        assert!(cost[0].contains("Speakers for Trill alone") && cost[0].contains("other programs make no sound"));

        let rate = AudioConfig { sample_rate: Some(44_100), ..AudioConfig::default() };
        let cost = said(&rate, false);
        assert_eq!(cost, ["Going from 48 kHz to 44.1 kHz stops playback and closes any open plugin windows."]);
        let same = AudioConfig { sample_rate: Some(48_000), ..AudioConfig::default() };
        assert_eq!(said(&same, false), ["The sound stops for a moment while the device is opened."]);

        // A buffer size asked of a shared device is not what it will get.
        let buffer = AudioConfig { buffer_size: Some(128), ..AudioConfig::default() };
        assert!(said(&buffer, false)[0].contains("honored in exclusive mode"));
        let both = AudioConfig { buffer_size: Some(128), exclusive: true, ..AudioConfig::default() };
        assert_eq!(said(&both, false).len(), 1, "exclusive, the size is honored and only the taking is said");

        // A device that cannot be taken says so instead.
        let hdmi = AudioConfig { output: Some("HDMI".into()), exclusive: true, ..AudioConfig::default() };
        assert!(said(&hdmi, false)[0].starts_with("HDMI cannot be taken for one program alone"));
        // And a take that is running ends.
        let other = AudioConfig { output: Some("Interface".into()), ..AudioConfig::default() };
        assert_eq!(said(&other, true).last().unwrap(), "The take being recorded ends when the device changes.");
        // A device nobody has listed may run at any rate.
        let unknown = AudioConfig { output: Some("Elsewhere".into()), ..AudioConfig::default() };
        assert!(costs(&unknown, &now, None, false)[0].starts_with("If that device runs at another sample rate"));
    }

    #[test]
    fn exclusive_mode_already_had_is_not_announced_again() {
        let devices = devices();
        let asked = AudioConfig { exclusive: true, ..AudioConfig::default() };
        let now = running(asked.clone());
        let smaller = AudioConfig { buffer_size: Some(64), ..asked.clone() };
        assert_eq!(
            costs(&smaller, &now, Some(&devices), false),
            ["The sound stops for a moment while the device is opened."]
        );
        // Taken on another device, it is another device that goes silent.
        let moved = AudioConfig { output: Some("Interface".into()), ..asked };
        assert!(costs(&moved, &now, Some(&devices), false)[0].contains("Interface for Trill alone"));
    }

    #[test]
    fn what_runs_is_said_in_one_line_as_the_device_reports_it() {
        let now = running(AudioConfig::default());
        assert_eq!(
            running_line(&now),
            "48 kHz \u{00B7} 480 frames \u{00B7} 10.0 ms \u{00B7} shared \u{00B7} heard 31.9 ms later, as the device reports"
        );
        assert_eq!(buffer_label(256, 48_000.0), "256 frames \u{00B7} 5.3 ms");
        assert_eq!(buffer_label(256, 0.0), "256 frames");
        assert_eq!(rates(&AudioConfig::default(), Some(&devices())), [44_100, 48_000]);
        assert_eq!(rates(&AudioConfig::default(), None), COMMON_SAMPLE_RATES, "until a device says what it takes");
        assert_eq!(draft_rate(&AudioConfig::default(), Some(&devices())), Some(48_000));
        assert_eq!(draft_rate(&AudioConfig::default(), None), None);
    }

    #[test]
    fn the_engines_words_are_set_as_sentences() {
        assert_eq!(
            sentence("there is no audio device called 'Interface'"),
            "There is no audio device called 'Interface'."
        );
        assert_eq!(sentence("  Already one. "), "Already one.");
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn a_stream_that_fell_back_says_why_and_what_happens_next() {
        let wanted = AudioConfig { output: Some("Interface".into()), ..AudioConfig::default() };
        let now = running(wanted);
        let words = fallen_back(&now, "there is no audio device called 'Interface'; using Speakers instead");
        assert_eq!(
            words,
            "There is no audio device called 'Interface'; using Speakers instead. Trill goes back to Interface when \
             it is connected again."
        );
        // The system's default has no name to wait for.
        let plain = running(AudioConfig::default());
        assert_eq!(fallen_back(&plain, "the device refused 96000 Hz"), "The device refused 96000 Hz.");
        // And a device that is in use, only not as asked, is not waited for.
        let here = running(AudioConfig { output: Some("Speakers".into()), ..AudioConfig::default() });
        assert_eq!(fallen_back(&here, "opened shared"), "Opened shared.");
    }

    #[test]
    fn a_midi_input_says_what_it_is_doing() {
        let input = |enabled, present, open, problem: Option<&str>| MidiInputInfo {
            name: "Keystation".into(),
            enabled,
            present,
            open,
            problem: problem.map(str::to_owned),
            events: 0,
        };
        assert_eq!(midi_state(&input(true, true, true, None)), ("Listening".to_owned(), false));
        assert_eq!(midi_state(&input(false, true, false, None)), ("Off".to_owned(), false));
        assert_eq!(midi_state(&input(false, false, false, None)), ("Off \u{00B7} not plugged in".to_owned(), false));
        assert_eq!(midi_state(&input(true, false, false, None)), ("Not plugged in".to_owned(), false));
        assert_eq!(midi_state(&input(true, true, false, None)), ("Opening\u{2026}".to_owned(), false));
        let (state, problem) = midi_state(&input(true, true, false, Some("another program is using this port")));
        assert_eq!(state, "Not listening: another program is using this port.");
        assert!(problem);
    }
}
