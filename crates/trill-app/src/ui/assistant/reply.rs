//! A reply as it is set in the panel: its Markdown read into blocks
//! ([`markdown`](super::markdown)), each block wrapped once and kept.
//!
//! Setting text is the costly part of showing it, and a reply arrives a few
//! words at a time, many times a second. So what was set is kept, reply by
//! reply and block by block:
//!
//! * A reply that has not changed is not looked at again: neither read nor
//!   set. A frame costs it one comparison.
//! * A reply that has grown is read again (reading is cheap), and of its
//!   blocks only those that differ from before are set: while a reply is
//!   being written that is its last block, the one growing. The blocks
//!   before it are, by how an open reply is read, already final.
//!
//! Nothing here runs unless the panel is being drawn.
//!
//! Every block is running text of the kit's, so a reply can be selected
//! with the mouse across its paragraphs and copied.

use super::markdown::{self, BULLET, Block, Span};
use eframe::egui::{Galley, Id, Rect, Response, Role, Ui, WidgetInfo, pos2, vec2};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use trill_noir::prose::{self, Face, Run};
use trill_noir::{TextStyle, Theme, paint};

/// Space between a list's items, after a heading, and between other blocks.
/// The last is more than half a line, which is what makes a selection
/// copied across two paragraphs come out with an empty line between them;
/// the others are less, so a list is copied line under line.
const ITEM_GAP: f32 = 3.0;
const HEADING_GAP: f32 = 4.0;
const BLOCK_GAP: f32 = 10.0;
/// How far each level of a nested list is set in, and the least room for
/// what stands before an item.
const NESTING: f32 = 14.0;
const MARK: f32 = 16.0;
/// Space inside the frame of a block of code.
const CODE_PAD: (f32, f32) = (8.0, 6.0);
/// How far a quote is set in from the rule beside it.
const QUOTE_INDENT: f32 = 10.0;
/// Height a rule takes.
const RULE: f32 = 9.0;
/// Blocks further than this outside the part of the conversation on show
/// are not painted. Those nearer are, though they cannot be seen: text
/// being selected must still be there when its end has scrolled out of
/// sight, or the selection would be lost with it.
const KEPT_BEYOND: f32 = 1600.0;

/// What kind of thing a set block is, for how it is spaced and dressed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Heading,
    Item,
    Code,
    Quote,
    Rule,
}

/// One block, set.
struct Placed {
    /// What it was set from, hashed: the block, and the width, scale and
    /// theme it was set for.
    hash: u64,
    kind: Kind,
    /// The words, wrapped; `None` for a rule.
    galley: Option<Arc<Galley>>,
    /// What stands before an item of a list, and whether it is part of
    /// what is read (a number is, a dash is not).
    mark: Option<(Arc<Galley>, bool)>,
    /// From the reply's top to the block's.
    top: f32,
    /// From the column's left edge to the words.
    left: f32,
    height: f32,
    /// The words without their faces, for assistive technology.
    label: String,
}

/// What a reply was set from and for. While this holds, it stands.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Key {
    length: usize,
    open: bool,
    width: f32,
    scale: f32,
    theme: &'static str,
}

/// A reply, set.
pub struct Laid {
    key: Key,
    blocks: Vec<Placed>,
    pub height: f32,
}

/// The replies of the conversation on show, as they were last set.
#[derive(Default)]
pub struct Replies {
    /// The conversation they are of (see `Transcript::epoch`).
    epoch: u64,
    /// By the reply's place in the conversation.
    laid: Vec<Option<Laid>>,
    /// How many blocks have been set since the panel came up: what the
    /// tests count to see that nothing is set twice.
    pub blocks_set: u64,
}

fn hash_of(block: &Block, key: &Key) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    block.hash(&mut hasher);
    key.width.to_bits().hash(&mut hasher);
    key.scale.to_bits().hash(&mut hasher);
    key.theme.hash(&mut hasher);
    hasher.finish()
}

fn runs<'a>(spans: &'a [Span], over: impl Fn(Face) -> Face) -> Vec<Run<'a>> {
    spans.iter().map(|span| Run { face: over(span.face), text: &span.text }).collect()
}

/// Sets one block in a column `width` wide with `edge` to spare on either
/// side.
fn place(ui: &Ui, theme: &Theme, block: &Block, hash: u64, width: f32, edge: f32) -> Placed {
    let body = theme.text.body;
    let room = width - 2.0 * edge;
    let keep = |face: Face| face;
    let (kind, galley, mark, left, label) = match block {
        Block::Paragraph(spans) => {
            let galley = prose::layout(ui, theme, body, theme.ink, &runs(spans, keep), room);
            (Kind::Text, Some(galley), None, edge, markdown::text_of(spans))
        }
        Block::Heading { level, spans } => {
            // The first two levels as loud as the text gets; the rest a
            // size down and quieter.
            let (style, color) = match level {
                1 | 2 => (body, theme.ink),
                _ => (TextStyle { size: 12.0, ..body }, theme.ink_dim),
            };
            let strong = |face: Face| Face { strong: true, ..face };
            let galley = prose::layout(ui, theme, style, color, &runs(spans, strong), room);
            (Kind::Heading, Some(galley), None, edge, markdown::text_of(spans))
        }
        Block::Item { mark, depth, spans } => {
            let read = mark != BULLET;
            let mark = prose::plain(ui, theme, body, theme.ink_dim, mark, room);
            let indent = *depth as f32 * NESTING;
            let cell = (mark.size().x + 6.0).max(MARK);
            let left = edge + indent + cell;
            let galley = prose::layout(ui, theme, body, theme.ink, &runs(spans, keep), width - edge - left);
            (Kind::Item, Some(galley), Some((mark, read)), left, markdown::text_of(spans))
        }
        Block::Code(code) => {
            let galley = prose::code_block(ui, body, theme.ink, code, room - 2.0 * CODE_PAD.0);
            (Kind::Code, Some(galley), None, edge + CODE_PAD.0, code.clone())
        }
        Block::Quote(spans) => {
            let galley = prose::layout(ui, theme, body, theme.ink_dim, &runs(spans, keep), room - QUOTE_INDENT);
            (Kind::Quote, Some(galley), None, edge + QUOTE_INDENT, markdown::text_of(spans))
        }
        Block::Rule => (Kind::Rule, None, None, edge, String::new()),
    };
    let height = match (&galley, kind) {
        (Some(galley), Kind::Code) => galley.size().y + 2.0 * CODE_PAD.1,
        (Some(galley), _) => galley.size().y,
        (None, _) => RULE,
    };
    Placed { hash, kind, galley, mark, top: 0.0, left, height, label }
}

/// The space above a block that follows one of kind `above`.
fn gap(above: Kind, block: Kind) -> f32 {
    match (above, block) {
        (Kind::Item, Kind::Item) => ITEM_GAP,
        (Kind::Heading, _) => HEADING_GAP,
        _ => BLOCK_GAP,
    }
}

impl Replies {
    /// Call before the replies of a frame are asked for: what was set for
    /// another conversation than `epoch`, or for items this one no longer
    /// has, is let go of.
    pub fn begin(&mut self, epoch: u64, items: usize) {
        if self.epoch != epoch {
            self.laid.clear();
            self.epoch = epoch;
        }
        self.laid.truncate(items);
    }

    /// The reply at `index` of the conversation, set for a column `width`
    /// wide: as it was set before if nothing about it has changed, and
    /// otherwise set anew where it differs. A reply only ever grows, so its
    /// length says whether it has changed. `open` says it is still being
    /// written.
    #[expect(clippy::too_many_arguments, reason = "what a reply is set from and for, each a plain value")]
    pub fn laid(
        &mut self,
        ui: &Ui,
        theme: &Theme,
        index: usize,
        text: &str,
        open: bool,
        width: f32,
        edge: f32,
    ) -> &Laid {
        if self.laid.len() <= index {
            self.laid.resize_with(index + 1, || None);
        }
        let scale = ui.pixels_per_point();
        let key = Key { length: text.len(), open, width, scale, theme: theme.name };
        let entry = &mut self.laid[index];
        if entry.as_ref().is_some_and(|laid| laid.key == key) {
            return entry.as_ref().expect("it was just seen");
        }
        let mut placed = entry.take().map(|laid| laid.blocks).unwrap_or_default();
        let blocks = markdown::parse(text, open);
        let mut top = 0.0;
        for (at, block) in blocks.iter().enumerate() {
            let hash = hash_of(block, &key);
            if placed.get(at).is_none_or(|old| old.hash != hash) {
                let new = place(ui, theme, block, hash, width, edge);
                self.blocks_set += 1;
                match placed.get_mut(at) {
                    Some(old) => *old = new,
                    None => placed.push(new),
                }
            }
            if at > 0 {
                top += gap(placed[at - 1].kind, placed[at].kind);
            }
            placed[at].top = top;
            top += placed[at].height;
        }
        placed.truncate(blocks.len());
        entry.insert(Laid { key, blocks: placed, height: top })
    }
}

/// Paints a reply set by [`Replies::laid`] with its top left corner at
/// `origin`, in a column `width` wide, and returns the response of each
/// block of text painted. `id` is the reply's own.
pub fn paint(
    ui: &Ui,
    theme: &Theme,
    laid: &Laid,
    origin: eframe::egui::Pos2,
    width: f32,
    edge: f32,
    id: Id,
) -> Vec<Response> {
    let mut responses = Vec::new();
    let near = ui.clip_rect().expand2(vec2(0.0, KEPT_BEYOND));
    for (index, block) in laid.blocks.iter().enumerate() {
        let top = origin.y + block.top;
        let whole = Rect::from_min_size(pos2(origin.x, top), vec2(width, block.height));
        if !near.intersects(whole) {
            continue;
        }
        let painter = ui.painter();
        match block.kind {
            Kind::Code => {
                let well = whole.shrink2(vec2(edge, 0.0));
                painter.rect_filled(well, 0.0, theme.panel);
                paint::frame(painter, well, theme.hairline);
            }
            Kind::Quote => {
                let bar = Rect::from_min_size(pos2(origin.x + edge, top), vec2(2.0, block.height));
                painter.rect_filled(bar, 0.0, theme.ghost);
            }
            Kind::Rule => {
                let x = (origin.x + edge)..=(origin.x + width - edge);
                paint::hline(painter, x, top + (RULE / 2.0).floor(), theme.hairline);
            }
            Kind::Text | Kind::Heading | Kind::Item => {}
        }
        if let Some((mark, read)) = &block.mark {
            let at = pos2(origin.x + block.left - mark.size().x - 6.0, top);
            match read {
                true => drop(prose::selectable(ui, id.with((index, "mark")), at, mark.clone())),
                false => prose::paint(ui, at, mark.clone()),
            }
        }
        let Some(galley) = &block.galley else { continue };
        let inset = if block.kind == Kind::Code { CODE_PAD.1 } else { 0.0 };
        let response = prose::selectable(ui, id.with(index), pos2(origin.x + block.left, top + inset), galley.clone());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &block.label));
        responses.push(response);
    }
    responses
}
