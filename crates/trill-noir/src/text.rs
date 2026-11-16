//! Laying out and placing text in the kit's styles.
//!
//! egui shapes text without OpenType features, so Inter's `tnum` cannot be
//! switched on, and it has no letter spacing outside a [`LayoutJob`]. Both
//! are done here: tracking through the job's letter spacing, and tabular
//! figures by giving every digit a cell as wide as the widest digit and
//! centering the digit in it, which is what the font's own tabular set does.

use crate::theme::TextStyle;
use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat, TextWrapping};
use egui::{Align2, Color32, FontId, Galley, Painter, Rect, pos2, vec2};
use std::sync::Arc;

/// The minus sign of the kit: U+2212, as wide as a plus, never a hyphen.
pub const MINUS: char = '\u{2212}';

/// One line of text, laid out and ready to [`place`].
pub struct Line {
    galley: Arc<Galley>,
    /// Space after the last glyph that belongs to the line: half the cell
    /// padding of a final tabular digit. Without it "1" and "8" would end
    /// at different places when right-aligned.
    trailing: f32,
}

impl Line {
    /// The width the line occupies.
    pub fn width(&self) -> f32 {
        self.galley.size().x + self.trailing
    }

    /// True if the text did not fit and ends in an ellipsis.
    pub fn elided(&self) -> bool {
        self.galley.elided
    }

    /// The text as set (in capitals for uppercase styles).
    pub fn text(&self) -> &str {
        self.galley.text()
    }
}

/// Lays out one line of `text` in `style`. Text wider than `max_width` ends
/// in an ellipsis.
pub fn layout(painter: &Painter, style: TextStyle, text: &str, color: Color32, max_width: f32) -> Line {
    let font = FontId::proportional(style.size);
    let shown = if style.upper { text.to_uppercase() } else { text.to_owned() };
    let format =
        TextFormat { font_id: font.clone(), color, extra_letter_spacing: style.tracking, ..Default::default() };

    let mut job = LayoutJob::default();
    let mut trailing = 0.0;
    if style.tabular && shown.bytes().any(|byte| byte.is_ascii_digit()) {
        let advances = painter.fonts_mut(|fonts| DIGITS.map(|digit| fonts.glyph_width(&font, digit)));
        trailing = append_tabular(&mut job, &shown, &format, &advances);
    } else {
        job.append(&shown, 0.0, format);
    }
    job.wrap = TextWrapping::truncate_at_width((max_width - trailing).max(0.0));
    let galley = painter.layout_job(job);
    // A cut line ends in an ellipsis, not in the digit the padding was for.
    let trailing = if galley.elided { 0.0 } else { trailing };
    Line { galley, trailing }
}

const DIGITS: [char; 10] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];

/// Appends `text` with each digit centered in a cell of the widest digit's
/// width, and returns the padding still owed after the last character.
///
/// Every digit is a section of its own, placed by the space before it;
/// everything between digits stays one section, so letters keep their
/// kerning. (egui applies letter spacing across section boundaries, so the
/// tracking needs no help.)
///
/// Sections are pushed directly: [`LayoutJob::append`] merges a section
/// that needs no leading space into the one before it, and a digit shaped
/// together with its neighbor is kerned against it ("4." closes up), which
/// is exactly what tabular figures must not do.
fn append_tabular(job: &mut LayoutJob, text: &str, format: &TextFormat, advances: &[f32; 10]) -> f32 {
    let cell = advances.iter().copied().fold(0.0, f32::max);
    // Space owed before the next section: the right half of the previous
    // digit's padding.
    let mut owed = 0.0;
    let mut rest = text;
    while !rest.is_empty() {
        if rest.as_bytes()[0].is_ascii_digit() {
            let padding = (cell - advances[(rest.as_bytes()[0] - b'0') as usize]) / 2.0;
            push_section(job, &rest[..1], owed + padding, format);
            owed = padding;
            rest = &rest[1..];
        } else {
            let run = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
            push_section(job, &rest[..run], owed, format);
            owed = 0.0;
            rest = &rest[run..];
        }
    }
    owed
}

fn push_section(job: &mut LayoutJob, text: &str, leading_space: f32, format: &TextFormat) {
    let start = job.text.len();
    job.text.push_str(text);
    let byte_range = ByteIndex(start)..ByteIndex(job.text.len());
    job.sections.push(LayoutSection { leading_space, byte_range, format: format.clone() });
}

/// Paints `line` in `rect`, anchored at `align`, clipped to the rect.
pub fn place(painter: &Painter, rect: Rect, align: Align2, line: Line) {
    let size = vec2(line.width(), line.galley.size().y);
    let position = align.anchor_size(align.pos_in_rect(&rect), size).min;
    // Whole pixels: a glyph that straddles two rows of pixels goes soft.
    let scale = painter.pixels_per_point();
    let position = pos2((position.x * scale).round() / scale, (position.y * scale).round() / scale);
    painter.with_clip_rect(rect.intersect(painter.clip_rect())).galley(position, line.galley, Color32::PLACEHOLDER);
}

/// Lays out one line of `text` to fit `rect` and paints it anchored at
/// `align`. Returns the width the text took.
pub fn paint(painter: &Painter, rect: Rect, align: Align2, style: TextStyle, text: &str, color: Color32) -> f32 {
    if rect.width() <= 0.0 || text.is_empty() {
        return 0.0;
    }
    let line = layout(painter, style, text, color, rect.width());
    let width = line.width();
    place(painter, rect, align, line);
    width
}

/// Width of one line of `text` in `style`, untruncated.
pub fn width(painter: &Painter, style: TextStyle, text: &str) -> f32 {
    layout(painter, style, text, Color32::PLACEHOLDER, f32::INFINITY).width()
}

/// What stands for the part cut out of a name.
pub const ELLIPSIS: char = '\u{2026}';

/// `text` made to fit `max_width` by taking out of its middle: `Kick_Deep`
/// from a pack called `LongPackName_Kick_Deep_01` reads `Lon\u{2026}Kick_Deep_01`.
/// For file names, which tend to start alike (with the name of the pack
/// they came in) and differ further on, so that cutting the end (as
/// [`layout`] does) leaves a list of identical rows. A quarter of what is
/// kept is the start and the rest the end, where the kind of sound and its
/// number usually are. Text that fits comes back as it is.
pub fn elide_middle(painter: &Painter, style: TextStyle, text: &str, max_width: f32) -> String {
    if width(painter, style, text) <= max_width {
        return text.to_owned();
    }
    let letters: Vec<char> = text.chars().collect();
    let cut = |kept: usize| -> String {
        let head = kept / 4;
        let tail = kept - head;
        letters[..head].iter().chain([&ELLIPSIS]).chain(&letters[letters.len() - tail..]).collect()
    };
    // The most letters that fit, found by halving: a handful of layouts,
    // which the painter remembers from frame to frame.
    let (mut fits, mut too_many) = (0, letters.len());
    while too_many - fits > 1 {
        let kept = (fits + too_many) / 2;
        if width(painter, style, &cut(kept)) <= max_width {
            fits = kept;
        } else {
            too_many = kept;
        }
    }
    cut(fits)
}

/// Like [`paint`], for a name: text too wide for `rect` loses its middle
/// rather than its end (see [`elide_middle`]).
pub fn paint_name(painter: &Painter, rect: Rect, align: Align2, style: TextStyle, text: &str, color: Color32) -> f32 {
    if rect.width() <= 0.0 || text.is_empty() {
        return 0.0;
    }
    paint(painter, rect, align, style, &elide_middle(painter, style, text, rect.width()), color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;
    use crate::testing::with_ui;

    #[test]
    fn names_lose_their_middle_and_keep_both_ends() {
        with_ui(|ui, theme| {
            let painter = ui.painter();
            let style = theme.text.body;
            let name = "LongPackName_drum_loop_140_swing_Ebm";
            let whole = width(painter, style, name);
            assert_eq!(elide_middle(painter, style, name, whole + 1.0), name, "what fits is left alone");
            assert_eq!(elide_middle(painter, style, "", 50.0), "");

            let cut = elide_middle(painter, style, name, whole * 0.6);
            assert!(width(painter, style, &cut) <= whole * 0.6, "{cut}");
            let (head, tail) = cut.split_once(ELLIPSIS).expect("an ellipsis where the middle was");
            assert!(name.starts_with(head) && name.ends_with(tail), "{cut}");
            assert!(head.len() >= 3 && tail.len() >= 2 * head.len(), "mostly the end, some of the start: {cut}");
            // As long as it can be: one more letter would not fit.
            let more: String = format!("{head}{}{ELLIPSIS}{tail}", &name[head.len()..=head.len()]);
            assert!(width(painter, style, &more) > whole * 0.6);
            // Two names that differ only at the end stay different.
            let other = elide_middle(painter, style, "LongPackName_drum_loop_140_swing_Fm", whole * 0.6);
            assert_ne!(cut, other);

            // With no room at all, the ellipsis alone.
            assert_eq!(elide_middle(painter, style, name, 0.0), ELLIPSIS.to_string());
            assert_eq!(
                elide_middle(painter, style, "\u{00FC}ber_n\u{00EF}ce_name_here", 40.0).chars().count() > 1,
                true
            );
        });
    }

    #[test]
    fn digits_share_one_width_in_tabular_styles() {
        with_ui(|ui, theme| {
            let painter = ui.painter();
            let value = theme.text.value;
            let ones = width(painter, value, "1111");
            let eights = width(painter, value, "8888");
            assert!((ones - eights).abs() < 0.01, "1111 is {ones}, 8888 is {eights}");
            // Without the treatment Inter's 1 is much narrower than its 8.
            let proportional = TextStyle { tabular: false, ..value };
            assert!(width(painter, proportional, "8888") - width(painter, proportional, "1111") > 4.0);

            // Mixed text keeps the property for its numeric part.
            let a = width(painter, value, "\u{2212}11.1 dB");
            let b = width(painter, value, "\u{2212}48.8 dB");
            assert!((a - b).abs() < 0.01, "{a} vs {b}");
            // And when it ends in a digit, which is what right-aligned
            // columns of numbers do.
            let a = width(painter, value, "17. 1. 1");
            let b = width(painter, value, "48. 4. 4");
            assert!((a - b).abs() < 0.01, "{a} vs {b}");
            // Inter kerns "4." and "7."; a tabular digit must not move for
            // its neighbor.
            let a = width(painter, value, "44.4 kHz");
            let b = width(painter, value, "88.8 kHz");
            assert!((a - b).abs() < 0.01, "{a} vs {b}");
        });
    }

    #[test]
    fn tracking_widens_text_by_its_gaps() {
        with_ui(|ui, theme| {
            let caps = theme.text.caps;
            let tight = TextStyle { tracking: 0.0, ..caps };
            let spread = width(ui.painter(), caps, "FILTER") - width(ui.painter(), tight, "FILTER");
            // Five gaps between six letters, give or take pixel rounding.
            assert!((spread - 5.0 * caps.tracking).abs() < 0.6, "tracking added {spread}");

            // Tabular text is tracked the same, digit sections included.
            let value = theme.text.value;
            let tight = TextStyle { tracking: 0.0, ..value };
            let spread = width(ui.painter(), value, "12.5 kHz") - width(ui.painter(), tight, "12.5 kHz");
            assert!((spread - 7.0 * value.tracking).abs() < 0.6, "tracking added {spread}");
        });
    }

    #[test]
    fn uppercase_styles_set_capitals_and_long_text_is_cut() {
        with_ui(|ui, theme| {
            let painter = ui.painter();
            let line = layout(painter, theme.text.caps, "Amp Envelope", Color32::WHITE, f32::INFINITY);
            assert_eq!(line.text(), "AMP ENVELOPE");
            let body = layout(painter, theme.text.body, "Amp Envelope", Color32::WHITE, f32::INFINITY);
            assert_eq!(body.text(), "Amp Envelope");
            assert!(!body.elided());

            let cut = layout(painter, theme.text.body, "A very long project name indeed", Color32::WHITE, 60.0);
            assert!(cut.width() <= 60.0, "{}", cut.width());
            assert!(cut.elided());
            let cut = layout(painter, theme.text.value, "1234567890123456", Color32::WHITE, 40.0);
            assert!(cut.width() <= 40.0 && cut.elided(), "{}", cut.width());
        });
    }

    #[test]
    fn the_theme_types_are_usable_as_given() {
        let theme = Theme::noir();
        assert!(theme.text.value.tabular && theme.text.readout.tabular);
        assert!(theme.text.caps.upper && theme.text.micro.upper && !theme.text.body.upper);
    }
}
