//! Markdown, as far as the assistant writes it in a narrow panel:
//! paragraphs, **strong** and *emphasized* words, `code` and fenced code
//! blocks, bullet and numbered lists, headings, quotes, rules, and links,
//! which are shown as text (the words, then the address in brackets) and
//! open nothing. Tables, images and HTML are left as they were typed.
//!
//! A reply is read while it is still being written, so [`parse`] takes
//! whether the text is `open`: more of it may yet come. An open text is
//! read so that **nothing shown is ever taken back**:
//!
//! * The end of the last line is held back while it could still turn out
//!   to be markup: a `*` that may become `**`, a `-` or `1.` that may begin
//!   a list, a `#`, the backticks of a fence, a `[` whose link has not
//!   closed. It appears with the character that settles what it is.
//! * A word that has begun strong, emphasized or as code is shown so from
//!   its first letter, although its closing mark has not arrived.
//!
//! Every block but the last is then already what it will be when the text
//! is complete, and the last one only grows. That is what lets the panel
//! set each block once and keep it (see `view`), and it is why a reply
//! does not flicker between `**dou` and **doubled** as it arrives. The
//! tests check it letter by letter.
//!
//! This is not CommonMark. Where the two differ it is on purpose and
//! towards what a chat panel wants: a line break inside a paragraph is
//! kept, nesting is by indentation alone, and what is not understood is
//! shown as typed rather than guessed at.

use trill_noir::prose::Face;

/// A stretch of a block's text in one face.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub face: Face,
    pub text: String,
}

/// A piece of a reply, as it is set.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Block {
    Paragraph(Vec<Span>),
    /// `level` 1 for `#` to 6 for `######`.
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    /// An item of a list, with what stands before it (a dash, or its
    /// number) and how deep it is nested, from 0.
    Item {
        mark: String,
        depth: u8,
        spans: Vec<Span>,
    },
    /// A fenced block, set as it was typed.
    Code(String),
    Quote(Vec<Span>),
    /// A line across.
    Rule,
}

/// What stands before an item of a bullet list.
pub const BULLET: &str = "\u{2013}";
/// Lists are shown nested this deep and no deeper.
const MAX_DEPTH: u8 = 3;
/// An unclosed `[` is waited on for this many characters before it is
/// taken for an ordinary bracket, and the address after it for this many.
const LINK_LABEL_PATIENCE: usize = 120;
const LINK_ADDRESS_PATIENCE: usize = 400;

/// Collects the characters of a block into spans, a span per run of one
/// face.
#[derive(Default)]
struct Spans(Vec<Span>);

impl Spans {
    fn push(&mut self, face: Face, c: char) {
        match self.0.last_mut() {
            Some(last) if last.face == face => last.text.push(c),
            _ => self.0.push(Span { face, text: c.to_string() }),
        }
    }

    fn push_str(&mut self, face: Face, text: &str) {
        text.chars().for_each(|c| self.push(face, c));
    }

    fn text(&self) -> String {
        self.0.iter().map(|span| span.text.as_str()).collect()
    }
}

/// The text of `spans` without its faces.
pub fn text_of(spans: &[Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

fn run_length(chars: &[char], at: usize) -> usize {
    chars[at..].iter().take_while(|c| **c == chars[at]).count()
}

/// Where the run of `count` backticks that closes a code span opened
/// before `from` begins.
fn closing_backticks(chars: &[char], from: usize, count: usize) -> Option<usize> {
    let mut at = from;
    while at < chars.len() {
        if chars[at] == '`' {
            let run = run_length(chars, at);
            if run == count {
                return Some(at);
            }
            at += run;
        } else {
            at += 1;
        }
    }
    None
}

/// Whether a run of `mark` at `at`, `run` long, may open strong or
/// emphasized text, and whether it may close it: it must touch a word on
/// that side, and an underscore inside a word is only an underscore.
fn flanking(chars: &[char], at: usize, run: usize, mark: char) -> (bool, bool) {
    let before = at.checked_sub(1).map(|index| chars[index]);
    let after = chars.get(at + run).copied();
    let opens = after.is_some_and(|c| !c.is_whitespace());
    let closes = before.is_some_and(|c| !c.is_whitespace());
    if mark == '_' {
        let in_word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        (opens && !in_word(before), closes && !in_word(after))
    } else {
        (opens, closes)
    }
}

/// Whether a run of at least `count` of `mark` that may close follows
/// `from`, outside code.
fn closes_later(chars: &[char], from: usize, mark: char, count: usize) -> bool {
    let mut at = from;
    while at < chars.len() {
        match chars[at] {
            '\\' => at += 2,
            '`' => {
                let run = run_length(chars, at);
                at = closing_backticks(chars, at + run, run).map_or(at + run, |end| end + run);
            }
            c if c == mark => {
                let run = run_length(chars, at);
                if run >= count && flanking(chars, at, run, mark).1 {
                    return true;
                }
                at += run;
            }
            _ => at += 1,
        }
    }
    false
}

/// A link written `[words](address)` at `at`: where its words end, and
/// where its address begins and ends.
fn link_at(chars: &[char], at: usize) -> Option<(usize, usize, usize)> {
    let close = (at + 1..chars.len()).find(|index| matches!(chars[*index], ']' | '[' | '\n'))?;
    if chars[close] != ']' || chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = (close + 2..chars.len()).find(|index| matches!(chars[*index], ')' | '\n'))?;
    (chars[end] == ')').then_some((close, close + 2, end))
}

/// An address written `<https://...>` at `at`: where it ends.
fn address_at(chars: &[char], at: usize) -> Option<usize> {
    let end = (at + 1..chars.len()).find(|index| matches!(chars[*index], '>' | ' ' | '\n' | '<'))?;
    let address: String = chars[at + 1..end].iter().collect();
    (chars[end] == '>'
        && (address.starts_with("http://") || address.starts_with("https://") || address.starts_with("mailto:")))
    .then_some(end)
}

/// Reads the marks inside a block. With `hopeful`, a word whose closing
/// mark has not come is shown as begun; otherwise a mark without its
/// partner is what was typed.
fn inline(text: &str, hopeful: bool) -> Vec<Span> {
    let mut out = Spans::default();
    read_inline(&text.chars().collect::<Vec<_>>(), Face::PLAIN, hopeful, &mut out);
    // Nothing is shown after the last word.
    if let Some(last) = out.0.last_mut() {
        let kept = last.text.trim_end().len();
        last.text.truncate(kept);
        if last.text.is_empty() {
            out.0.pop();
        }
    }
    out.0
}

fn read_inline(chars: &[char], base: Face, hopeful: bool, out: &mut Spans) {
    let mut face = base;
    // The mark each was opened with: `*` closes `*`, `_` closes `_`.
    let (mut strong_mark, mut emphasis_mark) = ('*', '*');
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        match c {
            '\\' if chars.get(at + 1).is_some_and(char::is_ascii_punctuation) => {
                out.push(face, chars[at + 1]);
                at += 2;
            }
            // Two spaces or a backslash before a line break only ask for the break.
            '\\' if chars.get(at + 1) == Some(&'\n') => at += 1,
            ' ' if chars[at..].iter().take_while(|c| **c == ' ').count() >= 2
                && chars[at..].iter().find(|c| **c != ' ') == Some(&'\n') =>
            {
                at += chars[at..].iter().take_while(|c| **c == ' ').count();
            }
            '`' => {
                let run = run_length(chars, at);
                let code = Face { code: true, ..Face::PLAIN };
                match closing_backticks(chars, at + run, run) {
                    Some(end) => {
                        let mut inside = &chars[at + run..end];
                        // `` `code` `` may be padded to hold a backtick.
                        if inside.len() > 2
                            && inside[0] == ' '
                            && inside[inside.len() - 1] == ' '
                            && inside.iter().any(|c| *c != ' ')
                        {
                            inside = &inside[1..inside.len() - 1];
                        }
                        inside.iter().for_each(|c| out.push(code, if *c == '\n' { ' ' } else { *c }));
                        at = end + run;
                    }
                    None if hopeful => {
                        chars[at + run..].iter().for_each(|c| out.push(code, *c));
                        at = chars.len();
                    }
                    None => {
                        (0..run).for_each(|_| out.push(face, '`'));
                        at += run;
                    }
                }
            }
            '*' | '_' => {
                let run = run_length(chars, at);
                let (opens, closes) = flanking(chars, at, run, c);
                let mut left = run;
                if closes {
                    // The inner one first: `***both***` opens strong, then
                    // emphasis, and closes them the other way round.
                    if left >= 1 && face.emphasis && emphasis_mark == c && (left != 2 || !face.strong) {
                        face.emphasis = false;
                        left -= 1;
                    }
                    if left >= 2 && face.strong && strong_mark == c {
                        face.strong = false;
                        left -= 2;
                    }
                    if left >= 1 && face.emphasis && emphasis_mark == c {
                        face.emphasis = false;
                        left -= 1;
                    }
                }
                if opens {
                    let follows = at + run;
                    if left >= 2 && !face.strong && (closes_later(chars, follows, c, 2) || hopeful) {
                        face.strong = true;
                        strong_mark = c;
                        left -= 2;
                    }
                    if left >= 1 && !face.emphasis && (closes_later(chars, follows, c, 1) || hopeful) {
                        face.emphasis = true;
                        emphasis_mark = c;
                        left -= 1;
                    }
                }
                (0..left).for_each(|_| out.push(face, c));
                at += run;
            }
            '[' => match link_at(chars, at) {
                Some((words_end, address_start, end)) => {
                    let before = out.text().len();
                    read_inline(&chars[at + 1..words_end], face, false, out);
                    let words = out.text()[before..].to_owned();
                    let address: String = chars[address_start..end].iter().collect();
                    let address = address.split_whitespace().next().unwrap_or_default();
                    if words.is_empty() {
                        out.push_str(face, address);
                    } else if !address.is_empty() && !words.contains(address) {
                        out.push_str(face, &format!(" ({address})"));
                    }
                    at = end + 1;
                }
                None => {
                    out.push(face, c);
                    at += 1;
                }
            },
            '<' => match address_at(chars, at) {
                Some(end) => {
                    chars[at + 1..end].iter().for_each(|c| out.push(face, *c));
                    at = end + 1;
                }
                None => {
                    out.push(face, c);
                    at += 1;
                }
            },
            _ => {
                out.push(face, c);
                at += 1;
            }
        }
    }
}

// ---- lines ----------------------------------------------------------------------

/// What a line is, read by itself.
#[derive(Debug, PartialEq)]
enum Line<'a> {
    Blank,
    /// The line that opens or closes a fenced block: its mark and how many.
    Fence(char, usize),
    Heading(u8, &'a str),
    Rule,
    Item {
        mark: String,
        depth: u8,
        text: &'a str,
    },
    Quote(&'a str),
    Text(&'a str),
}

/// How far a line is indented, a tab counting as four, and the rest of it.
fn indented(line: &str) -> (usize, &str) {
    let rest = line.trim_start_matches([' ', '\t']);
    let indent = line[..line.len() - rest.len()].chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
    (indent, rest)
}

/// A line made of one of `marks` repeated, spaces between allowed: how
/// many times it is there.
fn repeated(line: &str, marks: &[char]) -> Option<(char, usize)> {
    let first = line.chars().next().filter(|c| marks.contains(c))?;
    line.chars().all(|c| c == first || c == ' ').then(|| (first, line.chars().filter(|c| *c == first).count()))
}

/// The number and what follows of a line that begins `12. ` or `12) `.
fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &line[digits..];
    if !(1..=9).contains(&digits) || !(rest.starts_with('.') || rest.starts_with(')')) {
        return None;
    }
    let after = &rest[1..];
    (after.is_empty() || after.starts_with(' ')).then(|| (&line[..digits], after.trim_start()))
}

fn classify(line: &str) -> Line<'_> {
    let (indent, rest) = indented(line);
    let rest = rest.trim_end();
    if rest.is_empty() {
        return Line::Blank;
    }
    let depth = ((indent / 2) as u8).min(MAX_DEPTH);
    if indent <= 3 {
        let mark = rest.chars().next().unwrap_or(' ');
        let fence = rest.chars().take_while(|c| *c == mark).count();
        if matches!(mark, '`' | '~') && fence >= 3 && !rest[fence..].contains('`') {
            return Line::Fence(mark, fence);
        }
        let level = rest.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=6).contains(&level) && (rest.len() == level || rest[level..].starts_with(' ')) {
            let title = rest[level..].trim().trim_end_matches('#').trim_end();
            return Line::Heading(level as u8, title);
        }
        if repeated(rest, &['-', '*', '_']).is_some_and(|(_, count)| count >= 3) {
            return Line::Rule;
        }
        if let Some(quoted) = rest.strip_prefix('>') {
            return Line::Quote(quoted.trim_start());
        }
    }
    if let Some(text) = ["- ", "* ", "+ ", "\u{2022} "].iter().find_map(|mark| rest.strip_prefix(mark)) {
        return Line::Item { mark: BULLET.to_owned(), depth, text: text.trim_start() };
    }
    if let Some((number, text)) = numbered(rest).filter(|(_, text)| !text.is_empty()) {
        return Line::Item { mark: format!("{number}."), depth, text };
    }
    Line::Text(rest)
}

/// Whether the last line of an open text, `line`, could still become the
/// mark of a block with the next character: it is not shown until it has.
fn may_become_a_mark(line: &str) -> bool {
    let (_, rest) = indented(line);
    if rest.is_empty() {
        return true;
    }
    let level = rest.bytes().take_while(|byte| *byte == b'#').count();
    let heading = (1..=6).contains(&level) && matches!(&rest[level..], "" | " ");
    let number = numbered(rest.trim_end_matches(' ')).is_some_and(|(_, text)| text.is_empty())
        || (rest.len() <= 9 && rest.bytes().all(|byte| byte.is_ascii_digit()));
    let fence = rest.starts_with("```") || rest.starts_with("~~~") || repeated(rest, &['`', '~']).is_some();
    heading || number || fence || repeated(rest, &['-', '*', '_', '=', '+']).is_some() || matches!(rest, ">" | "> ")
}

/// How much of the end of `line`, the last of an open text, to hold back
/// because it may yet turn out to be markup: marks at its very end, and a
/// link that has not closed.
fn held_back(line: &str) -> usize {
    let settled = line.trim_end_matches(['*', '_', '`', '~', '\\']);
    let mut keep = settled.len();
    if let Some(open) = settled.rfind('[') {
        let after = &settled[open + 1..];
        let waiting = match after.find(']') {
            // No end to the words yet.
            None => after.chars().count() <= LINK_LABEL_PATIENCE,
            // The words have ended: an address may follow, or has begun.
            Some(close) => match &after[close + 1..] {
                "" => true,
                rest => rest.starts_with('(') && !rest.contains(')') && rest.chars().count() <= LINK_ADDRESS_PATIENCE,
            },
        };
        if waiting {
            keep = open;
        }
    }
    if let Some(open) = settled[..keep].rfind('<') {
        let after = &settled[open + 1..keep];
        let address = "https://".starts_with(after) || "http://".starts_with(after) || after.starts_with("http");
        if address && !after.contains(['>', ' ']) && after.chars().count() <= LINK_ADDRESS_PATIENCE {
            keep = open;
        }
    }
    line.len() - settled[..keep].trim_end().len()
}

/// A block whose lines are still being gathered.
enum Gathering {
    Nothing,
    Paragraph(String),
    Item { mark: String, depth: u8, text: String },
    Quote(String),
}

impl Gathering {
    /// Adds a line to the block, if it has one to add to.
    fn extend(&mut self, line: &str) -> bool {
        match self {
            Gathering::Nothing => false,
            Gathering::Paragraph(text) | Gathering::Item { text, .. } | Gathering::Quote(text) => {
                text.push('\n');
                text.push_str(line);
                true
            }
        }
    }

    fn finish(&mut self, hopeful: bool, blocks: &mut Vec<Block>) {
        match std::mem::replace(self, Gathering::Nothing) {
            Gathering::Nothing => {}
            Gathering::Paragraph(text) => blocks.push(Block::Paragraph(inline(&text, hopeful))),
            Gathering::Item { mark, depth, text } => {
                blocks.push(Block::Item { mark, depth, spans: inline(&text, hopeful) });
            }
            Gathering::Quote(text) => blocks.push(Block::Quote(inline(&text, hopeful))),
        }
    }
}

/// Reads `source` into blocks. `open` says that more of it may come: see
/// the module's notes for what that changes.
pub fn parse(source: &str, open: bool) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut gathering = Gathering::Nothing;
    // The fence of the code block being read, and its lines so far.
    let mut fenced: Option<(char, usize, String)> = None;
    let unfinished = open && !source.ends_with('\n');
    let count = source.lines().count();

    for (index, line) in source.lines().enumerate() {
        let last = unfinished && index + 1 == count;
        if let Some((mark, length, code)) = &mut fenced {
            let (indent, rest) = indented(line);
            let closing = repeated(rest.trim_end(), &[*mark]).map(|(_, count)| count);
            if indent <= 3 && closing.is_some_and(|count| count >= *length) && !rest.contains(' ') {
                let code = std::mem::take(code);
                blocks.push(Block::Code(code.trim_end_matches('\n').to_owned()));
                fenced = None;
            } else if !(last && closing.is_some()) {
                // A last line of nothing but the fence's mark may be the
                // fence closing.
                code.push_str(line);
                code.push('\n');
            }
            continue;
        }
        // What is left of the last line once its unsettled end is held
        // back may itself be no more than the beginning of a mark.
        let line = if last { &line[..line.len() - held_back(line)] } else { line };
        if last && may_become_a_mark(line) {
            break;
        }
        match classify(line) {
            Line::Blank => gathering.finish(false, &mut blocks),
            Line::Fence(mark, length) => {
                gathering.finish(false, &mut blocks);
                fenced = Some((mark, length, String::new()));
            }
            Line::Heading(level, title) => {
                gathering.finish(false, &mut blocks);
                blocks.push(Block::Heading { level, spans: inline(title, last) });
            }
            Line::Rule => {
                gathering.finish(false, &mut blocks);
                blocks.push(Block::Rule);
            }
            Line::Item { mark, depth, text } => {
                gathering.finish(false, &mut blocks);
                gathering = Gathering::Item { mark, depth, text: text.to_owned() };
            }
            Line::Quote(text) => {
                if !matches!(gathering, Gathering::Quote(_)) {
                    gathering.finish(false, &mut blocks);
                    gathering = Gathering::Quote(String::new());
                }
                if let Gathering::Quote(quoted) = &mut gathering {
                    if !quoted.is_empty() {
                        quoted.push('\n');
                    }
                    quoted.push_str(text);
                }
            }
            Line::Text(text) => {
                if !gathering.extend(text) {
                    gathering = Gathering::Paragraph(text.to_owned());
                }
            }
        }
    }
    // What was being gathered when the text ran out is the last block: if
    // more may come, its marks are read hopefully.
    gathering.finish(open, &mut blocks);
    if let Some((_, _, code)) = fenced {
        blocks.push(Block::Code(code.trim_end_matches('\n').to_owned()));
    }
    // A block with nothing in it yet is not shown.
    blocks.retain(|block| match block {
        Block::Paragraph(spans) | Block::Quote(spans) | Block::Heading { spans, .. } => !spans.is_empty(),
        Block::Code(code) => !code.is_empty(),
        Block::Item { .. } | Block::Rule => true,
    });
    blocks
}

/// The reply as plain text, for copying: what is read, without the marks
/// that set it, a dash or a number before each item of a list.
pub fn plain_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            let listed = matches!((&blocks[index - 1], block), (Block::Item { .. }, Block::Item { .. }));
            out.push_str(if listed { "\n" } else { "\n\n" });
        }
        match block {
            Block::Paragraph(spans) | Block::Heading { spans, .. } => out.push_str(&text_of(spans)),
            Block::Item { mark, depth, spans } => {
                let mark = if mark == BULLET { "-" } else { mark };
                out.push_str(&"  ".repeat(*depth as usize));
                out.push_str(&format!("{mark} {}", text_of(spans)));
            }
            Block::Code(code) => out.push_str(code),
            Block::Quote(spans) => {
                let quoted = text_of(spans);
                out.push_str(&quoted.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n"));
            }
            Block::Rule => out.push_str("---"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(face: Face, text: &str) -> Span {
        Span { face, text: text.to_owned() }
    }

    fn plain(text: &str) -> Span {
        span(Face::PLAIN, text)
    }

    fn paragraph(text: &str) -> Block {
        Block::Paragraph(vec![plain(text)])
    }

    fn item(mark: &str, depth: u8, text: &str) -> Block {
        Block::Item { mark: mark.to_owned(), depth, spans: vec![plain(text)] }
    }

    const BOTH: Face = Face { strong: true, emphasis: true, ..Face::PLAIN };

    #[test]
    fn a_reply_is_read_as_paragraphs_lists_headings_and_code() {
        let reply = "## What I did\n\nI **doubled** the `hats`.\nThey run in *sixteenths* now.\n\n- Kick on one\n* Snare on \
                     two\n  - with a ghost note\n1. First\n12. Twelfth\n\n```text\nC  E  G\n  not a list. really\n```\nDone.\n\n";
        assert_eq!(
            parse(reply, false),
            [
                Block::Heading { level: 2, spans: vec![plain("What I did")] },
                Block::Paragraph(vec![
                    plain("I "),
                    span(Face::STRONG, "doubled"),
                    plain(" the "),
                    span(Face::CODE, "hats"),
                    plain(".\nThey run in "),
                    span(Face::EMPHASIS, "sixteenths"),
                    plain(" now."),
                ]),
                item(BULLET, 0, "Kick on one"),
                item(BULLET, 0, "Snare on two"),
                item(BULLET, 1, "with a ghost note"),
                item("1.", 0, "First"),
                item("12.", 0, "Twelfth"),
                Block::Code("C  E  G\n  not a list. really".into()),
                paragraph("Done."),
            ]
        );
        assert!(parse("", false).is_empty() && parse("\n\n", false).is_empty());
        // A sentence that merely contains a full stop is not a numbered
        // item, nor is a number with a decimal.
        assert_eq!(parse("Bar 5. Then bar 9.", false), [paragraph("Bar 5. Then bar 9.")]);
        assert_eq!(parse("3.5 dB down", false), [paragraph("3.5 dB down")]);
        assert_eq!(parse("---\n> quiet\n> part", false), [Block::Rule, Block::Quote(vec![plain("quiet\npart")])]);
        // A line under an item belongs to it.
        assert_eq!(
            parse("- one\n  and more\n- two", false),
            [item(BULLET, 0, "one\nand more"), item(BULLET, 0, "two")]
        );
        // Rows of a table stay as typed.
        assert_eq!(parse("| a | b |\n|---|---|", false), [paragraph("| a | b |\n|---|---|")]);
    }

    #[test]
    fn marks_inside_a_line_set_the_words_they_enclose() {
        let read = |text: &str| inline(text, false);
        assert_eq!(read("a **b** c"), [plain("a "), span(Face::STRONG, "b"), plain(" c")]);
        assert_eq!(read("__b__ and _c_"), [span(Face::STRONG, "b"), plain(" and "), span(Face::EMPHASIS, "c")]);
        assert_eq!(read("***both***"), [span(BOTH, "both")]);
        assert_eq!(
            read("**strong *and both* strong**"),
            [span(Face::STRONG, "strong "), span(BOTH, "and both"), span(Face::STRONG, " strong")]
        );
        // What touches no word is what was typed.
        assert_eq!(read("2 * 3 * 4"), [plain("2 * 3 * 4")]);
        assert_eq!(read("snake_case_name and 5*4"), [plain("snake_case_name and 5*4")]);
        // A mark without its partner too.
        assert_eq!(read("*args and **kwargs"), [plain("*args and **kwargs")]);
        assert_eq!(read("a ` b"), [plain("a ` b")]);
        // Escaped marks are the characters themselves.
        assert_eq!(read(r"\*not\* \_this\_ \\"), [plain(r"*not* _this_ \")]);
        // Code keeps what is in it.
        assert_eq!(read("`a **b**` c"), [span(Face::CODE, "a **b**"), plain(" c")]);
        assert_eq!(read("`` a`b ``"), [span(Face::CODE, "a`b")]);
        // Two spaces before a break only ask for the break.
        assert_eq!(read("one  \ntwo"), [plain("one\ntwo")]);
    }

    #[test]
    fn links_are_shown_as_text_with_their_address() {
        let read = |text: &str| inline(text, false);
        assert_eq!(
            read("See [the manual](https://example.com/manual) first."),
            [plain("See the manual (https://example.com/manual) first.")]
        );
        // The address is not said twice.
        assert_eq!(read("[https://example.com](https://example.com)"), [plain("https://example.com")]);
        assert_eq!(read("<https://example.com>"), [plain("https://example.com")]);
        assert_eq!(read("[](https://example.com)"), [plain("https://example.com")]);
        // Brackets that are not a link are brackets.
        assert_eq!(read("the [Verse] and a < b"), [plain("the [Verse] and a < b")]);
        assert_eq!(read("[a] (b)"), [plain("[a] (b)")]);
        assert_eq!(read("[**bold** words](x)"), [span(Face::STRONG, "bold"), plain(" words (x)")]);
    }

    #[test]
    fn copied_as_text_a_reply_reads_without_its_marks() {
        let reply =
            "# Title\nI **doubled** the `hats`.\n\n- one\n- two\n  1. deep\n\n```\ncode here\n```\n> quoted\n\n---";
        assert_eq!(
            plain_text(&parse(reply, false)),
            "Title\n\nI doubled the hats.\n\n- one\n- two\n  1. deep\n\ncode here\n\n> quoted\n\n---"
        );
        assert_eq!(plain_text(&[]), "");
    }

    /// The characters of a block with the face of each, and what kind of
    /// block it is.
    fn shown(block: &Block) -> (String, Vec<(Face, char)>) {
        let letters = |spans: &[Span]| -> Vec<(Face, char)> {
            spans.iter().flat_map(|span| span.text.chars().map(|c| (span.face, c))).collect()
        };
        match block {
            Block::Paragraph(spans) => ("paragraph".into(), letters(spans)),
            Block::Heading { level, spans } => (format!("heading {level}"), letters(spans)),
            Block::Item { mark, depth, spans } => (format!("item {mark} {depth}"), letters(spans)),
            Block::Code(code) => ("code".into(), code.chars().map(|c| (Face::CODE, c)).collect()),
            Block::Quote(spans) => ("quote".into(), letters(spans)),
            Block::Rule => ("rule".into(), Vec::new()),
        }
    }

    /// Reads `reply` as it arrives, cut after every character, and checks
    /// that nothing shown is taken back: every block but the last stays as
    /// it is, and the last only grows. Returns the blocks at the end.
    fn arriving(reply: &str) -> Vec<Block> {
        let mut before: Vec<Block> = Vec::new();
        let ends = reply.char_indices().map(|(index, c)| index + c.len_utf8());
        for end in ends {
            let now = parse(&reply[..end], true);
            let so_far = &reply[..end];
            assert!(now.len() >= before.len(), "a block went away after {so_far:?}: {before:?} then {now:?}");
            for (index, old) in before.iter().enumerate() {
                if index + 1 < before.len() {
                    assert_eq!(old, &now[index], "a finished block changed after {so_far:?}");
                } else {
                    let ((kind, old), (kind_now, new)) = (shown(old), shown(&now[index]));
                    assert_eq!(kind, kind_now, "the last block became another kind after {so_far:?}");
                    assert!(
                        new.starts_with(&old),
                        "the last block took something back after {so_far:?}: {old:?} then {new:?}"
                    );
                }
            }
            before = now;
        }
        before
    }

    #[test]
    fn a_reply_arriving_letter_by_letter_never_takes_back_what_it_showed() {
        for reply in [
            "I **doubled** the `hats` and *moved* the snare.",
            "## What I did\n\nI added an eight-bar line to **Bass** at bar 1.\n\nIt plays the *root* of each chord.",
            "Here is the plan:\n\n- **Kick** on every beat\n- *Hats* in sixteenths\n- `Clap` on 2 and 4\n\nShall I?",
            "1. First the drums\n2. Then the bass\n10. And so on\n\nDone.",
            "The chords:\n\n```\nAm | F | C | G\n```\n\nTwo bars each.",
            "```text\n* not a list\n# not a heading\n``\n```",
            "***Both*** at once, and __underscores__ too, and _these_.",
            "Read [the manual](https://example.com/a_b) or <https://example.com>.",
            "A line\nand the next\n\n> a quote\n> of two lines\n\n---\n\nThe end.",
            "- one\n  - nested **deep**\n    - deeper\n- back",
            "Use `a*b*c` and 2 * 3, bar 5. Then 3.5 dB.",
            "Transposed by \\*two\\* octaves, in D\u{266D}, caf\u{00E9} \u{2014} done.",
            "# Title\n## Sub\n### Third\nText right under it.",
        ] {
            let read = arriving(reply);
            assert_eq!(read, parse(reply, false), "read as it arrived, and read whole: {reply:?}");
            // And whole but still open, it is no different.
            assert_eq!(parse(reply, true), parse(reply, false), "{reply:?}");
        }
    }

    #[test]
    fn the_end_of_an_open_reply_waits_for_what_it_will_be() {
        let read = |text: &str| parse(text, true);
        // Marks that are not complete are not shown.
        assert_eq!(read("I *"), [paragraph("I")]);
        assert_eq!(read("I **"), [paragraph("I")]);
        assert_eq!(read("I **dou"), [Block::Paragraph(vec![plain("I "), span(Face::STRONG, "dou")])]);
        assert_eq!(read("I **doubled*"), [Block::Paragraph(vec![plain("I "), span(Face::STRONG, "doubled")])]);
        assert_eq!(read("the `ha"), [Block::Paragraph(vec![plain("the "), span(Face::CODE, "ha")])]);
        // The beginning of a line that may be a list, a heading or a fence.
        for waiting in
            ["-", "- ", "*", "**", "1", "1.", "1. ", "#", "## ", "`", "``", "```", "```rust", ">", "  ", "---"]
        {
            assert_eq!(read(&format!("Before.\n\n{waiting}")), [paragraph("Before.")], "{waiting:?}");
        }
        assert_eq!(read("Before.\n\n- K"), [paragraph("Before."), item(BULLET, 0, "K")]);
        assert_eq!(read("Before.\n\n1.5"), [paragraph("Before."), paragraph("1.5")]);
        assert_eq!(read("Before.\n\n-5 dB"), [paragraph("Before."), paragraph("-5 dB")]);
        assert_eq!(
            read("Before.\n\n# T"),
            [paragraph("Before."), Block::Heading { level: 1, spans: vec![plain("T")] }]
        );
        // A link is not shown until it is one, or cannot be.
        assert_eq!(read("See [the man"), [paragraph("See")]);
        assert_eq!(read("See [the manual]"), [paragraph("See")]);
        assert_eq!(read("See [the manual](https://exa"), [paragraph("See")]);
        assert_eq!(read("See [the manual] f"), [paragraph("See [the manual] f")]);
        assert_eq!(read("See <https://exa"), [paragraph("See")]);
        let long = format!("an [{}", "x".repeat(LINK_LABEL_PATIENCE + 1));
        assert_eq!(read(&long), [paragraph(&long)], "a bracket that never closes is not waited on for ever");
        // Inside a fence, only what may be its end waits.
        assert_eq!(read("```\nC E G\n``"), [Block::Code("C E G".into())]);
        assert_eq!(read("```\nC E G\n  x"), [Block::Code("C E G\n  x".into())]);
        // Once the reply is complete, nothing is held back.
        assert_eq!(parse("I **", false), [paragraph("I **")]);
        assert_eq!(parse("Before.\n\n-", false), [paragraph("Before."), paragraph("-")]);
        assert_eq!(parse("See [the man", false), [paragraph("See [the man")]);
    }

    #[test]
    fn a_reply_cut_anywhere_reads_the_same_once_it_is_whole() {
        // The deltas of a real reply fall anywhere: in the middle of a mark,
        // of a fence, of a character's worth of a list.
        let reply = "## Changes\n\n1. **Hats**: sixteenths from bar 9\n2. *Pad*: down `3 dB`\n\n```\nAm | F\n```\n";
        let whole = parse(reply, false);
        for first in 0..=reply.len() {
            for second in first..=reply.len() {
                if !reply.is_char_boundary(first) || !reply.is_char_boundary(second) {
                    continue;
                }
                // Three deltas: what was shown after each is a stage on the
                // way to the same end.
                let stages = [&reply[..first], &reply[..second], reply];
                let mut shown_before = 0;
                for stage in stages {
                    let read = parse(stage, true);
                    let letters: usize = read.iter().map(|block| shown(block).1.len()).sum();
                    assert!(letters >= shown_before, "less was shown after more arrived: {stage:?}");
                    shown_before = letters;
                }
                assert_eq!(parse(stages[2], true), whole);
            }
        }
    }
}
