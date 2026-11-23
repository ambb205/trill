//! The bar along the bottom: the state of the engine on the left, and on
//! the right a one-line hint for whatever is under the pointer.
//!
//! The first items are the audio stream as it is now: the device, its
//! sample rate, how much it asks for at a time and how late it is heard.
//! They follow the stream when it changes, and a click on them opens the
//! Audio and MIDI panel.

use super::audio_settings;
use super::missing;
use super::shortcuts::{self, Command};
use super::state::{AudioSummary, Cx};
use eframe::egui::{self, Align2, Color32, Painter, Rect, Sense, Ui, pos2};
use trill_core::device::{BUILTIN_SCHEME, split_uri};
use trill_noir::{TextStyle, fade, paint, text};

pub const HEIGHT: f32 = 22.0;
const EDGE: f32 = 10.0;
/// Space between items.
const GAP: f32 = 16.0;
/// What the hint says when the pointer is over nothing in particular.
const IDLE_HINT: &str = "Double-click a lane to create a clip \u{00B7} Space plays \u{00B7} F1 lists the shortcuts";
/// The hint never crowds the status items: it stops this far short of them.
const HINT_CLEARANCE: f32 = 32.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let bar = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline(&painter, bar.x_range(), bar.top(), theme.hairline);

    let mut row = Row { painter: &painter, cx, x: bar.left() + EDGE, bar };

    let audio = row.cx.audio;
    let (engine, state) = match (audio.silent(), row.cx.live.audio_faults, &audio.info.warning) {
        (true, ..) => (theme.warn, "No audio"),
        (false, faults, _) if faults > 0 => (theme.signal, "Engine"),
        // Sound, but not through what was asked for.
        (false, _, Some(_)) => (theme.warn, "Engine"),
        (false, _, None) => (theme.ok, "Engine"),
    };
    row.status(engine, state);
    for (label, value) in audio_readouts(audio) {
        row.readout(label, &value);
    }
    let audio_items = Rect::from_min_max(pos2(bar.left(), bar.top()), pos2(row.x - GAP + 6.0, bar.bottom()));
    // An input that is open hears the room: said for as long as it is,
    // whatever else the bar has to say.
    let live_item = row.live(ui);
    let plugins = plugin_count(row.cx);
    match (row.cx.catalog_ready, plugins) {
        (false, _) => row.status(theme.ink_faint, "Scanning plugins\u{2026}"),
        (true, 1) => row.status(theme.info, "1 plugin"),
        (true, count) => row.status(theme.info, &format!("{count} plugins")),
    }
    // A song that was just opened: how far its devices are.
    if let Some(words) = super::opening::status_words(row.cx) {
        row.status(theme.info, &words);
    }
    // A track this program is recording for the others, who have not got
    // its plugin: the machine is busy with it.
    let recording = row.cx.live.together.recording_for_others.as_deref();
    if let Some(words) = super::renditions::status_words(recording) {
        row.status(theme.info, &words);
    }
    // Tracks heard as someone else's Trill recorded them, because their
    // plugins are not on this computer.
    if let Some(words) = super::renditions::heard_words(&row.cx.snap.renditions) {
        row.status(theme.info, &words);
    }
    // Recordings on their way beside a song that was just saved, where
    // they have to be copied: how far that is.
    if let Some(coming) = row.cx.live.coming_beside {
        row.status(theme.info, &coming.said());
    }
    match row.cx.live.preparing {
        0 => {}
        1 => row.status(theme.info, "Preparing 1 warped clip\u{2026}"),
        count => row.status(theme.info, &format!("Preparing {count} warped clips\u{2026}")),
    }
    if let Some(path) = row.cx.jobs.exporting() {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        row.status(theme.info, &format!("Rendering {name}\u{2026}"));
    }
    if let Some((path, how_far)) = row.cx.filming.making() {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        row.status(theme.info, &format!("Filming {name} \u{00B7} {how_far}"));
    }
    // Who else is in the song, while it is shared: a click opens the panel.
    let together_item = super::together::status_item(ui, row.cx, &painter, row.x, bar).map(|(end, response)| {
        row.x = end + GAP;
        response
    });
    // A song that is more than the machine plays in time: one line for as
    // long as it is, which opens what each track costs.
    let short_item = row.short(ui);
    // Files the song names that are not on disk: one line for all of
    // them, which opens the list.
    let missing_item = row.missing(ui);
    // A newer Trill, where one is known and nothing was said against
    // hearing of it: one line, which opens what there is to read of it.
    let update_item = row.update(ui);
    let problems = problems(row.cx);
    let items_end = row.problems(ui, &problems);

    // The audio items are a button: they open the panel that changes them.
    let response = ui.interact(audio_items, ui.scope_id().with("audio"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Audio and MIDI settings"));
    if response.hovered() {
        painter.rect_filled(audio_items, 0.0, theme.hover);
    }
    let keys = shortcuts::keys(Command::AudioSettings);
    let about = match &cx.audio.info.warning {
        Some(warning) => format!("{warning} \u{00B7} click for the audio settings"),
        None => format!("Audio and MIDI settings: the device, its sample rate and buffer, MIDI inputs \u{00B7} {keys}"),
    };
    cx.hint(&response, about);
    if response.clicked() {
        audio_settings::open(cx);
    }
    if let Some(response) = together_item {
        cx.hint(&response, "Who is in this song with you \u{00B7} click for the invitation, and to leave");
        if response.clicked() {
            super::together::open_share(cx);
        }
    }
    if let Some(response) = live_item {
        let about = super::inputs::live_hint(cx);
        cx.hint(&response, about);
        if response.clicked() {
            audio_settings::open_on_input(cx);
        }
    }
    if let Some(response) = short_item {
        cx.hint(
            &response,
            "Buffers are coming late, each a gap in what is heard \u{00B7} click for what each track costs",
        );
        if response.clicked() {
            super::load::open(cx);
        }
    }
    if let Some(response) = missing_item {
        cx.hint(
            &response,
            "Files this song plays are not on disk \u{00B7} click to see them, what was found, and relink",
        );
        if response.clicked() {
            missing::open(cx);
        }
    }
    if let Some(response) = update_item {
        cx.hint(&response, "A newer Trill \u{00B7} click for what is new in it, to fetch it, or to hear no more of it");
        if response.clicked() {
            let ctx = ui.ctx().clone();
            super::setup::open_on_program(&ctx, cx);
        }
    }

    let hint = if cx.state.hint.text().is_empty() { IDLE_HINT } else { cx.state.hint.text() };
    let room = Rect::from_min_max(pos2(items_end + HINT_CLEARANCE, bar.top()), pos2(bar.right() - EDGE, bar.bottom()));
    text::paint(&painter, room, Align2::RIGHT_CENTER, theme.text.micro, hint, theme.ink_faint);
}

/// Lays status items out left to right.
struct Row<'a, 'cx> {
    painter: &'a Painter,
    cx: &'a Cx<'cx>,
    x: f32,
    bar: Rect,
}

impl Row<'_, '_> {
    fn rest(&self) -> Rect {
        Rect::from_min_max(pos2(self.x, self.bar.top()), self.bar.max)
    }

    /// A status dot and a word.
    fn status(&mut self, color: Color32, word: &str) {
        let theme = self.cx.theme;
        self.x = paint::dot(self.painter, pos2(self.x, self.bar.center().y), theme.metric.dot, color) + 5.0;
        self.x +=
            text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, word, theme.ink_faint) + GAP;
    }

    /// A label and its value.
    fn readout(&mut self, label: &str, value: &str) {
        let theme = self.cx.theme;
        self.x +=
            text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, label, theme.ink_faint) + 5.0;
        // Values in the bar are set like its labels, in capitals.
        let style = TextStyle { upper: true, ..theme.text.value };
        // A long device name gives way before it pushes the rest out.
        let room = self.rest().with_max_x(self.x + 300.0);
        self.x += text::paint(self.painter, room, Align2::LEFT_CENTER, style, value, theme.ink_dim) + GAP;
    }

    /// That the audio input is open, in the signal color: a button that
    /// opens the audio settings. `None` while it is closed.
    fn live(&mut self, ui: &Ui) -> Option<egui::Response> {
        let words = super::inputs::live_words(self.cx)?;
        let theme = self.cx.theme;
        let start = self.x;
        self.x = paint::dot(self.painter, pos2(self.x, self.bar.center().y), theme.metric.dot, theme.signal) + 5.0;
        self.x += text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, &words, theme.signal);
        let rect = Rect::from_min_max(pos2(start - 4.0, self.bar.top()), pos2(self.x + 4.0, self.bar.bottom()));
        self.x += GAP;
        let response = ui.interact(rect, ui.scope_id().with("input-live"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &words));
        if response.hovered() {
            self.painter.rect_filled(rect, 0.0, fade(theme.signal, 0.12));
        }
        Some(response)
    }

    /// That the song is more than the machine plays in time, in the signal
    /// color: a button that opens the playback load. `None` while it is
    /// played in time.
    fn short(&mut self, ui: &Ui) -> Option<egui::Response> {
        let words = super::load::status_words(&self.cx.live.keeping)?;
        let theme = self.cx.theme;
        let start = self.x;
        self.x = paint::dot(self.painter, pos2(self.x, self.bar.center().y), theme.metric.dot, theme.signal) + 5.0;
        self.x += text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, &words, theme.signal);
        let item = Rect::from_min_max(pos2(start - 4.0, self.bar.top()), pos2(self.x + 4.0, self.bar.bottom()));
        self.x += GAP;
        let response = ui.interact(item, ui.scope_id().with("falling-short"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, words.clone()));
        if response.hovered() {
            self.painter.rect_filled(item, 0.0, theme.hover);
        }
        Some(response)
    }

    /// What the song is missing, in the warning color: a button that
    /// opens the list. `None` when nothing is missing.
    fn missing(&mut self, ui: &Ui) -> Option<egui::Response> {
        let words = missing::status_words(&self.cx.sounds.missing.found)?;
        let theme = self.cx.theme;
        let start = self.x;
        self.x = paint::dot(self.painter, pos2(self.x, self.bar.center().y), theme.metric.dot, theme.warn) + 5.0;
        self.x += text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, &words, theme.warn);
        let item = Rect::from_min_max(pos2(start - 4.0, self.bar.top()), pos2(self.x + 4.0, self.bar.bottom()));
        self.x += GAP;
        let response = ui.interact(item, ui.scope_id().with("missing-files"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Missing files: {words}")));
        if response.hovered() {
            self.painter.rect_filled(item, 0.0, theme.hover);
        }
        Some(response)
    }

    /// That a newer Trill is out, or is fetched and waits for a start: a
    /// button that opens the tab of Setup that says more. `None` where
    /// none is known, or its user asked to hear no more of it.
    fn update(&mut self, ui: &Ui) -> Option<egui::Response> {
        let words = self.cx.setup.notice()?.short();
        let theme = self.cx.theme;
        let start = self.x;
        self.x = paint::dot(self.painter, pos2(self.x, self.bar.center().y), theme.metric.dot, theme.info) + 5.0;
        self.x += text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, &words, theme.info);
        let item = Rect::from_min_max(pos2(start - 4.0, self.bar.top()), pos2(self.x + 4.0, self.bar.bottom()));
        self.x += GAP;
        let response = ui.interact(item, ui.scope_id().with("newer-trill"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("A newer Trill: {words}")));
        if response.hovered() {
            self.painter.rect_filled(item, 0.0, theme.hover);
        }
        Some(response)
    }

    /// How many things need attention, in the warning color; hovering lists
    /// them. Returns the x where the items end.
    fn problems(&mut self, ui: &Ui, problems: &[String]) -> f32 {
        if problems.is_empty() {
            return self.x - GAP;
        }
        let theme = self.cx.theme;
        let start = self.x;
        let word = if problems.len() == 1 { "1 problem".to_owned() } else { format!("{} problems", problems.len()) };
        self.x = paint::dot(self.painter, pos2(self.x, self.bar.center().y), theme.metric.dot, theme.warn) + 5.0;
        self.x += text::paint(self.painter, self.rest(), Align2::LEFT_CENTER, theme.text.micro, &word, theme.warn);
        let item = Rect::from_min_max(pos2(start - 4.0, self.bar.top()), pos2(self.x + 4.0, self.bar.bottom()));
        ui.interact(item, ui.scope_id().with("problems"), Sense::hover()).on_hover_ui(|ui| {
            ui.set_max_width(440.0);
            ui.label(egui::RichText::new("Needs attention").color(theme.warn));
            for problem in problems {
                ui.label(egui::RichText::new(format!("\u{2022} {problem}")).color(theme.ink_dim));
            }
        });
        self.x
    }
}

/// Everything the problems item should tell the user about.
fn problems(cx: &Cx) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(warning) = &cx.audio.info.warning {
        problems.push(warning.clone());
    }
    // An input that something needs and that is not there, or not as it
    // was asked for.
    if let Some(problem) = &cx.live.input.problem {
        problems.push(problem.clone());
    }
    // A file that is not on disk is said by the item beside this one,
    // once for the song, and not here again for each clip that names it.
    problems.extend(cx.sounds.missing.found.other_problems(cx.snap.problems.clone()));
    if cx.live.audio_faults > 0 {
        problems.push(format!(
            "A device crashed {} time(s) while processing audio; those moments were silenced.",
            cx.live.audio_faults
        ));
    }
    problems
}

/// Third-party devices in the catalog.
fn plugin_count(cx: &Cx) -> usize {
    let is_plugin = |uri: &str| split_uri(uri).is_some_and(|(scheme, _)| scheme != BUILTIN_SCHEME);
    cx.descriptors.iter().filter(|descriptor| is_plugin(&descriptor.uri)).count()
}

/// The audio stream as the bar reads it out: the device and its sample
/// rate, and for a real device how much it asks for at a time and how long
/// after that is rendered it is heard, as the device reports.
fn audio_readouts(audio: &AudioSummary) -> Vec<(&'static str, String)> {
    let mut readouts = vec![("Out", output_name(audio).to_owned()), ("Rate", format_rate(audio.info.sample_rate))];
    if audio.info.device.is_some() {
        readouts.push(("Buffer", audio.info.buffer_frames.to_string()));
        readouts.push(("Latency", format!("{:.1} ms", audio.latency_ms)));
    }
    readouts
}

/// The name of the device in use, or how the engine describes going
/// without one.
fn output_name(audio: &AudioSummary) -> &str {
    audio.info.device.as_deref().unwrap_or_else(|| without_rate(&audio.info.output))
}

/// A description of the output without the sample rate the engine appends
/// to it, which has its own readout.
fn without_rate(output: &str) -> &str {
    let Some((name, rate)) = output.rsplit_once(" (") else { return output };
    let is_rate = rate.strip_suffix(" Hz)").is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()));
    if is_rate { name } else { output }
}

/// `"48 kHz"`, `"44.1 kHz"`.
pub(super) fn format_rate(sample_rate: f64) -> String {
    let khz = sample_rate / 1000.0;
    if (khz - khz.round()).abs() < 0.005 { format!("{khz:.0} kHz") } else { format!("{khz:.1} kHz") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_name_loses_its_rate_suffix_only() {
        assert_eq!(without_rate("Speakers (USB Audio Device) (48000 Hz)"), "Speakers (USB Audio Device)");
        assert_eq!(without_rate("Speakers (USB Audio Device)"), "Speakers (USB Audio Device)");
        assert_eq!(without_rate("none (null backend)"), "none (null backend)");
        assert_eq!(without_rate("Line Out"), "Line Out");
    }

    #[test]
    fn the_bar_reads_out_the_stream_that_is_running() {
        use trill_engine::{AudioConfig, StreamInfo};
        let mut audio = AudioSummary {
            config: AudioConfig::default(),
            info: StreamInfo {
                sample_rate: 44_100.0,
                output: "Speakers (Realtek) (44100 Hz)".into(),
                warning: None,
                device: Some("Speakers (Realtek)".into()),
                buffer_frames: 144,
                exclusive: true,
                latency_frames: 144,
            },
            latency_ms: 5.94,
            revision: 3,
        };
        let said = |audio: &AudioSummary| -> Vec<String> {
            audio_readouts(audio).into_iter().map(|(label, value)| format!("{label} {value}")).collect()
        };
        assert_eq!(said(&audio), ["Out Speakers (Realtek)", "Rate 44.1 kHz", "Buffer 144", "Latency 5.9 ms"]);
        assert!(!audio.silent());

        // The latency follows the device, without flickering in its last digit.
        audio.hear_latency(5.99);
        assert_eq!(audio.latency_ms, 5.94, "a hair's difference is not shown");
        audio.hear_latency(31.9);
        assert_eq!(said(&audio)[3], "Latency 31.9 ms");

        // A run without a device has no buffer or latency to speak of.
        audio.info.device = None;
        audio.info.output = "none (null backend)".into();
        assert_eq!(said(&audio), ["Out none (null backend)", "Rate 44.1 kHz"]);
        assert!(!audio.silent(), "it was asked to go without sound");
        audio.info.warning = Some("no audio output".into());
        assert!(audio.silent(), "a device was wanted and none could be opened");
    }

    #[test]
    fn sample_rates_read_in_kilohertz() {
        assert_eq!(format_rate(48_000.0), "48 kHz");
        assert_eq!(format_rate(44_100.0), "44.1 kHz");
        assert_eq!(format_rate(88_200.0), "88.2 kHz");
        assert_eq!(format_rate(96_000.0), "96 kHz");
    }
}
