//! Running text: wrapped to a width, set in more than one face, and
//! selectable with the mouse.
//!
//! [`text`](crate::text) sets single lines: labels, values, names. This is
//! for passages that are read, and that someone may want to take a part of
//! with them: a reply, an explanation, an error in a program's own words.
//!
//! A passage is a list of [`Run`]s, each in a [`Face`]: plain, strong
//! (Inter SemiBold), emphasized (slanted), code (the monospaced face, on a
//! wash) or a link (underlined; nothing here opens anything). [`layout`]
//! wraps them at a width, and [`selectable`] paints the result so that it
//! can be selected like text in any document: drag across it, double-click
//! a word, triple-click a line, Shift-click to extend, and Ctrl+C. A
//! selection runs across every passage painted this way in the same panel,
//! so several paragraphs can be taken at once; copied, passages a line
//! apart are a line apart and passages further apart get an empty line
//! between them. The selection itself is egui's, so it behaves as text
//! does everywhere else egui shows any.
//!
//! Laying out is the caller's to keep: a galley costs nothing to paint
//! again, and a passage that has not changed should not be set again.

use crate::theme::{TextStyle, Theme};
use egui::text::{LayoutJob, TextFormat};
use egui::text_selection::LabelSelectionState;
use egui::{Color32, FontFamily, FontId, Galley, Id, Pos2, Rect, Response, Sense, Stroke, Ui, pos2};
use std::sync::Arc;

/// The font family strong text is set in. Registered by [`crate::install`].
pub(crate) const STRONG: &str = "noir-strong";

/// Line height of running text, as a multiple of its size.
pub const LEADING: f32 = 1.42;

/// How a run of running text is set. The faces combine, except that code is
/// always upright and of one weight.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Face {
    /// Heavier: Inter SemiBold.
    pub strong: bool,
    /// Slanted.
    pub emphasis: bool,
    /// In the monospaced face, on a wash.
    pub code: bool,
    /// Underlined, as a link is. It is shown as one and does nothing.
    pub link: bool,
}

impl Face {
    pub const PLAIN: Face = Face { strong: false, emphasis: false, code: false, link: false };
    pub const STRONG: Face = Face { strong: true, ..Face::PLAIN };
    pub const EMPHASIS: Face = Face { emphasis: true, ..Face::PLAIN };
    pub const CODE: Face = Face { code: true, ..Face::PLAIN };
}

/// A stretch of text in one face.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Run<'a> {
    pub face: Face,
    pub text: &'a str,
}

impl<'a> Run<'a> {
    pub fn plain(text: &'a str) -> Self {
        Self { face: Face::PLAIN, text }
    }
}

/// The size code is set at beside text of `size`: the monospaced face runs
/// large, so it is taken down to sit level with the words around it.
fn code_size(size: f32) -> f32 {
    (size * 0.92 * 2.0).round() / 2.0
}

/// The font a face is set in at `size`.
fn font(face: Face, size: f32) -> FontId {
    if face.code {
        FontId::new(code_size(size), FontFamily::Monospace)
    } else if face.strong {
        FontId::new(size, FontFamily::Name(STRONG.into()))
    } else {
        FontId::proportional(size)
    }
}

/// Wraps `runs` at `width`, in `style` and `color`. The line height is the
/// same for every face, so a word of code does not push its line apart.
pub fn layout(ui: &Ui, theme: &Theme, style: TextStyle, color: Color32, runs: &[Run], width: f32) -> Arc<Galley> {
    let line_height = (style.size * LEADING).round();
    let mut job = LayoutJob::default();
    for run in runs.iter().filter(|run| !run.text.is_empty()) {
        let face = run.face;
        let format = TextFormat {
            font_id: font(face, style.size),
            color,
            extra_letter_spacing: if face.code { 0.0 } else { style.tracking },
            line_height: Some(line_height),
            italics: face.emphasis && !face.code,
            background: if face.code { theme.fill } else { Color32::TRANSPARENT },
            underline: if face.link { Stroke::new(1.0, theme.ink_faint) } else { Stroke::NONE },
            ..Default::default()
        };
        if style.upper && !face.code {
            job.append(&run.text.to_uppercase(), 0.0, format);
        } else {
            job.append(run.text, 0.0, format);
        }
    }
    job.wrap.max_width = width.max(40.0);
    ui.painter().layout_job(job)
}

/// Wraps `words` at `width` in one face.
pub fn plain(ui: &Ui, theme: &Theme, style: TextStyle, color: Color32, words: &str, width: f32) -> Arc<Galley> {
    layout(ui, theme, style, color, &[Run::plain(words)], width)
}

/// `position` moved onto the pixel grid: a glyph that straddles two rows of
/// pixels goes soft.
fn on_pixels(ui: &Ui, position: Pos2) -> Pos2 {
    let scale = ui.painter().pixels_per_point();
    pos2((position.x * scale).round() / scale, (position.y * scale).round() / scale)
}

/// Paints `galley` at `position` as text that can be selected with the
/// mouse and copied. `id` must be the passage's own, and the same from
/// frame to frame: a selection is remembered by it.
///
/// Passages are selected across in the order they are painted, so paint
/// them top to bottom (and, on one line, left to right).
pub fn selectable(ui: &Ui, id: Id, position: Pos2, galley: Arc<Galley>) -> Response {
    let position = on_pixels(ui, position);
    let rect = Rect::from_min_size(position, galley.size());
    // Never by the Tab key: a passage is read, not operated.
    let mut sense = Sense::click_and_drag();
    sense -= Sense::FOCUSABLE;
    let response = ui.interact(rect, id, sense);
    LabelSelectionState::label_text_selection(ui, &response, position, galley, Color32::PLACEHOLDER, Stroke::NONE);
    response
}

/// Paints `galley` at `position` as text that is only read.
pub fn paint(ui: &Ui, position: Pos2, galley: Arc<Galley>) {
    ui.painter().galley(on_pixels(ui, position), galley, Color32::PLACEHOLDER);
}

/// Whether some running text is selected anywhere in the window. While it
/// is, Ctrl+C means that text.
pub fn has_selection(ctx: &egui::Context) -> bool {
    ctx.plugin_opt::<LabelSelectionState>().is_some_and(|state| state.lock().has_selection())
}

/// Drops the selection, if there is one.
pub fn clear_selection(ctx: &egui::Context) {
    if let Some(state) = ctx.plugin_opt::<LabelSelectionState>() {
        state.lock().clear_selection();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{context, frame, with_ui};
    use egui::{Event, Modifiers, PointerButton, vec2};

    fn width_of(ui: &Ui, theme: &Theme, face: Face, words: &str) -> f32 {
        layout(ui, theme, theme.text.body, theme.ink, &[Run { face, text: words }], 1000.0).size().x
    }

    #[test]
    fn strong_text_is_heavier_and_code_is_monospaced() {
        with_ui(|ui, theme| {
            let plain = width_of(ui, theme, Face::PLAIN, "Doubled the hats");
            let strong = width_of(ui, theme, Face::STRONG, "Doubled the hats");
            assert!(strong > plain + 2.0, "SemiBold sets wider than Regular: {strong} against {plain}");
            // Slanting moves no letter.
            assert_eq!(width_of(ui, theme, Face::EMPHASIS, "Doubled the hats"), plain);

            let narrow = width_of(ui, theme, Face::CODE, "iiii");
            let wide = width_of(ui, theme, Face::CODE, "mmmm");
            assert!((narrow - wide).abs() < 0.01, "code is set in one width: {narrow} against {wide}");
            assert!(width_of(ui, theme, Face::PLAIN, "mmmm") - width_of(ui, theme, Face::PLAIN, "iiii") > 10.0);
        });
    }

    #[test]
    fn a_passage_wraps_at_its_width_on_lines_of_one_height() {
        with_ui(|ui, theme| {
            let words = "The hats now run in sixteenths through the second half of the verse, a little quieter.";
            let runs = [
                Run::plain("The hats now run in "),
                Run { face: Face::STRONG, text: "sixteenths" },
                Run::plain(" through "),
                Run { face: Face::CODE, text: "Beat A" },
                Run::plain(", a little quieter than before so that the snare keeps its place."),
            ];
            let style = theme.text.body;
            let galley = layout(ui, theme, style, theme.ink, &runs, 180.0);
            assert!(galley.size().x <= 180.0, "{}", galley.size().x);
            assert!(galley.rows.len() >= 3, "{} rows", galley.rows.len());
            let line = (style.size * LEADING).round();
            let height = galley.size().y;
            let rows = galley.rows.len() as f32;
            assert!((height - rows * line).abs() < 1.0, "{rows} rows of {line} make {height}");
            // The text is the runs' own, joined: what a copy takes.
            assert!(galley.text().starts_with("The hats now run in sixteenths through Beat A, a little"));

            // One face, the same thing in fewer words.
            let single = plain(ui, theme, style, theme.ink, words, 180.0);
            assert_eq!(single.text(), words);
            // An uppercase style sets capitals, but never code.
            let caps = layout(ui, theme, theme.text.caps, theme.ink, &runs[1..4], 400.0);
            assert_eq!(caps.text(), "SIXTEENTHS THROUGH Beat A");
        });
    }

    /// A press, a move or a release of the main button at `at`.
    fn pointer(at: Pos2, pressed: Option<bool>) -> Vec<Event> {
        match pressed {
            Some(pressed) => {
                vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]
            }
            None => vec![Event::PointerMoved(at)],
        }
    }

    /// Two passages, one under the other, shown for one frame with
    /// `events`. Returns where they were.
    fn two_passages(ctx: &egui::Context, time: &mut f64, events: Vec<Event>) -> [Rect; 2] {
        *time += 1.0 / 60.0;
        let origin = pos2(20.0, 20.0);
        let mut rows = [Rect::NOTHING; 2];
        frame(ctx, *time, events, |ui, theme| {
            let first = plain(ui, theme, theme.text.body, theme.ink, "Kick on one", 300.0);
            let second = plain(ui, theme, theme.text.body, theme.ink, "Snare on two", 300.0);
            let below = origin + vec2(0.0, first.size().y + 10.0);
            rows[0] = selectable(ui, Id::new("first"), origin, first).rect;
            rows[1] = selectable(ui, Id::new("second"), below, second).rect;
        });
        rows
    }

    #[test]
    fn dragging_across_passages_selects_them_and_a_click_elsewhere_lets_go() {
        let ctx = context();
        let mut time = 0.0;
        let mut show = |events: Vec<Event>| two_passages(&ctx, &mut time, events);
        show(Vec::new());
        let rows = show(Vec::new());
        assert!(!has_selection(&ctx));

        // From the start of the first passage to the end of the second.
        let (from, to) = (rows[0].left_center() + vec2(0.5, 0.0), rows[1].right_center() - vec2(0.5, 0.0));
        show(pointer(from, None));
        show(pointer(from, Some(true)));
        show(pointer(from + (to - from) * 0.5, None));
        show(pointer(to, None));
        show(pointer(to, Some(false)));
        show(Vec::new());
        assert!(has_selection(&ctx), "a drag across the two passages selected something");

        // A click on nothing lets go of it.
        let elsewhere = pos2(380.0, 180.0);
        show(pointer(elsewhere, None));
        show(pointer(elsewhere, Some(true)));
        show(pointer(elsewhere, Some(false)));
        show(Vec::new());
        assert!(!has_selection(&ctx));

        // And so does asking.
        show(pointer(from, None));
        show(pointer(from, Some(true)));
        show(pointer(to, None));
        show(pointer(to, Some(false)));
        assert!(has_selection(&ctx));
        clear_selection(&ctx);
        assert!(!has_selection(&ctx));
    }
}
