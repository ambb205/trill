//! People: how someone else who has the song open is shown in it.
//!
//! Each person has one of the theme's [`people`](Theme::people) colours,
//! and everything of theirs is drawn in it: the outline of what they have
//! selected, the line where their song stands, the tag with their name.
//! The colours are louder than the track hues, which they are drawn over,
//! and keep clear of the signal colour, which is this program's own
//! playhead.

use crate::theme::{Theme, fade};
use crate::{paint, text};
use egui::{Align2, Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

/// How many colours people are told apart by.
pub const PEOPLE: usize = 8;

/// The height of a person's tag.
pub const TAG: f32 = 14.0;

impl Theme {
    /// The colour of the person whose colour is numbered `index`.
    pub fn person(&self, index: u8) -> Color32 {
        self.people[usize::from(index) % PEOPLE]
    }
}

/// The width a tag with `name` on it takes.
pub fn person_tag_width(painter: &Painter, theme: &Theme, name: &str) -> f32 {
    (text::width(painter, theme.text.micro, name) + 8.0).ceil()
}

/// A person's name on their colour, its top left corner at `at`. Returns
/// the rectangle it took.
pub fn person_tag(painter: &Painter, theme: &Theme, at: Pos2, name: &str, colour: Color32) -> Rect {
    let rect = Rect::from_min_size(at, vec2(person_tag_width(painter, theme, name), TAG));
    painter.rect_filled(rect, 0.0, colour);
    text::paint(painter, rect, Align2::CENTER_CENTER, theme.text.micro, name, theme.on_accent);
    rect
}

/// The mark that something is held by a person: an outline in their
/// colour, a point and a half wide and inside the rectangle, so that it
/// never covers a neighbour.
pub fn person_outline(painter: &Painter, rect: Rect, colour: Color32) {
    let inner = rect.shrink(0.75);
    painter.rect_stroke(inner, 0.0, Stroke::new(1.5, colour), egui::StrokeKind::Middle);
}

/// A row that a person has selected: a bar of their colour down its left
/// edge and a wash over the rest.
pub fn person_row(painter: &Painter, rect: Rect, colour: Color32) {
    painter.rect_filled(rect, 0.0, fade(colour, 0.07));
    painter.rect_filled(Rect::from_min_max(rect.min, pos2(rect.left() + 3.0, rect.bottom())), 0.0, colour);
}

/// Where a person's song stands: a line down `y` at `x` in their colour,
/// thinner and quieter than one's own playhead, with a flag at its top.
pub fn person_playhead(painter: &Painter, x: f32, y: std::ops::RangeInclusive<f32>, colour: Color32) {
    paint::vline(painter, x, y.clone(), fade(colour, 0.7));
    let tip = pos2(x + 0.5, *y.start());
    let flag = vec![tip + vec2(-3.5, 0.0), tip + vec2(3.5, 0.0), tip + vec2(0.0, 5.0)];
    painter.add(Shape::convex_polygon(flag, colour, Stroke::NONE));
}

/// A person in a list: a square of their colour.
pub fn person_dot(painter: &Painter, center: Pos2, colour: Color32) {
    painter.rect_filled(Rect::from_center_size(center, vec2(7.0, 7.0)), 0.0, colour);
}
