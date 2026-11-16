//! The bar along the bottom: the state of the engine on the left, and on
//! the right a one-line hint for whatever is under the pointer.

use super::state::Cx;
use eframe::egui::{self, Align2, Color32, Painter, Rect, Sense, Ui, pos2};
use trill_core::device::{BUILTIN_SCHEME, split_uri};
use trill_noir::{TextStyle, paint, text};

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

    let (engine, state) = match (&row.cx.audio.warning, row.cx.live.audio_faults) {
        (Some(_), _) => (theme.warn, "No audio"),
        (None, faults) if faults > 0 => (theme.signal, "Engine"),
        (None, _) => (theme.ok, "Engine"),
    };
    row.status(engine, state);
    row.readout("Out", output_name(&row.cx.audio.output));
    row.readout("Rate", &format_rate(row.cx.audio.sample_rate));
    let plugins = plugin_count(row.cx);
    match (row.cx.catalog_ready, plugins) {
        (false, _) => row.status(theme.ink_faint, "Scanning plugins\u{2026}"),
        (true, 1) => row.status(theme.info, "1 plugin"),
        (true, count) => row.status(theme.info, &format!("{count} plugins")),
    }
    if let Some(path) = row.cx.jobs.exporting() {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        row.status(theme.info, &format!("Rendering {name}\u{2026}"));
    }
    let problems = problems(row.cx);
    let items_end = row.problems(ui, &problems);

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
    if let Some(warning) = &cx.audio.warning {
        problems.push(warning.clone());
    }
    problems.extend(cx.snap.problems.iter().cloned());
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

/// The output's name without the sample rate the engine appends to it,
/// which has its own readout.
fn output_name(output: &str) -> &str {
    let Some((name, rate)) = output.rsplit_once(" (") else { return output };
    let is_rate = rate.strip_suffix(" Hz)").is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()));
    if is_rate { name } else { output }
}

/// `"48 kHz"`, `"44.1 kHz"`.
fn format_rate(sample_rate: f64) -> String {
    let khz = sample_rate / 1000.0;
    if (khz - khz.round()).abs() < 0.005 { format!("{khz:.0} kHz") } else { format!("{khz:.1} kHz") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_name_loses_its_rate_suffix_only() {
        assert_eq!(output_name("Speakers (Schiit Unison Modius E) (48000 Hz)"), "Speakers (Schiit Unison Modius E)");
        assert_eq!(output_name("Speakers (Schiit Unison Modius E)"), "Speakers (Schiit Unison Modius E)");
        assert_eq!(output_name("none (null backend)"), "none (null backend)");
        assert_eq!(output_name("Line Out"), "Line Out");
    }

    #[test]
    fn sample_rates_read_in_kilohertz() {
        assert_eq!(format_rate(48_000.0), "48 kHz");
        assert_eq!(format_rate(44_100.0), "44.1 kHz");
        assert_eq!(format_rate(88_200.0), "88.2 kHz");
        assert_eq!(format_rate(96_000.0), "96 kHz");
    }
}
