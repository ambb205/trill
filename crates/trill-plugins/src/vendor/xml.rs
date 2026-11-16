//! As much of XML as plugins write their states in: elements with
//! attributes, nested, and nothing between them that matters.
//!
//! Two things are done with such a document. One is changing the values
//! of attributes in place, leaving every other byte as the plugin wrote
//! it: [`tags`] says where each attribute's value is. The other is laying
//! one document over another: [`Element`] is a document as a tree, read
//! and written back.
//!
//! Text between tags, comments, processing instructions and CDATA are
//! passed over. A document that is not well formed stops the reading with
//! what is wrong; none makes it read past its end or nest without limit.

use std::ops::Range;

/// How deep elements may nest. States nest ten deep at most.
const MAX_DEPTH: usize = 256;
/// The most elements a document may have.
const MAX_ELEMENTS: usize = 1_000_000;

/// An attribute of a start tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attribute<'a> {
    pub name: &'a str,
    /// Its value as it is written, entities and all.
    pub raw: &'a str,
    /// Where `raw` is in the document.
    pub at: Range<usize>,
}

impl Attribute<'_> {
    pub(crate) fn value(&self) -> String {
        unescape(self.raw)
    }
}

/// A tag of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tag<'a> {
    /// `<name a="b">`, or `<name a="b"/>` if `closed`.
    Start { name: &'a str, attributes: Vec<Attribute<'a>>, closed: bool },
    /// `</name>`
    End { name: &'a str },
}

/// Walks the tags of a document.
pub(crate) struct Tags<'a> {
    text: &'a str,
    at: usize,
    failed: bool,
}

/// The tags of `text`, in order. The walk ends at the first thing that is
/// not well formed, with what it is.
pub(crate) fn tags(text: &str) -> Tags<'_> {
    Tags { text, at: 0, failed: false }
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
}

impl<'a> Tags<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    /// Skips to just past `end`, or says that `what` is not closed.
    fn skip_past(&mut self, end: &str, what: &str) -> Result<(), String> {
        match self.rest().find(end) {
            Some(found) => {
                self.at += found + end.len();
                Ok(())
            }
            None => Err(format!("a {what} is not closed")),
        }
    }

    fn name(&mut self) -> Result<&'a str, String> {
        let rest = self.rest();
        let length = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
        if length == 0 {
            return Err("a tag has no name".to_string());
        }
        self.at += length;
        Ok(&rest[..length])
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start().len();
    }

    fn start(&mut self) -> Result<Tag<'a>, String> {
        let name = self.name()?;
        let mut attributes = Vec::new();
        loop {
            self.skip_space();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.at += 2;
                return Ok(Tag::Start { name, attributes, closed: true });
            }
            if rest.starts_with('>') {
                self.at += 1;
                return Ok(Tag::Start { name, attributes, closed: false });
            }
            let attribute = self.name().map_err(|_| format!("the tag '{name}' is not closed"))?;
            self.skip_space();
            if !self.rest().starts_with('=') {
                return Err(format!("the attribute '{attribute}' of '{name}' has no value"));
            }
            self.at += 1;
            self.skip_space();
            let quote = self.rest().chars().next().filter(|c| matches!(c, '"' | '\''));
            let quote = quote.ok_or_else(|| format!("the value of '{attribute}' of '{name}' is not quoted"))?;
            self.at += 1;
            let length = self.rest().find(quote).ok_or_else(|| format!("the value of '{attribute}' is not closed"))?;
            let at = self.at..self.at + length;
            attributes.push(Attribute { name: attribute, raw: &self.text[at.clone()], at });
            self.at += length + 1;
        }
    }

    fn next_tag(&mut self) -> Result<Option<Tag<'a>>, String> {
        loop {
            let Some(found) = self.rest().find('<') else { return Ok(None) };
            self.at += found;
            let rest = self.rest();
            if rest.starts_with("<!--") {
                self.skip_past("-->", "comment")?;
            } else if rest.starts_with("<![CDATA[") {
                self.skip_past("]]>", "CDATA section")?;
            } else if rest.starts_with("<?") {
                self.skip_past("?>", "processing instruction")?;
            } else if rest.starts_with("<!") {
                self.skip_past(">", "declaration")?;
            } else if rest.starts_with("</") {
                self.at += 2;
                let name = self.name()?;
                self.skip_space();
                if !self.rest().starts_with('>') {
                    return Err(format!("the end of '{name}' is not closed"));
                }
                self.at += 1;
                return Ok(Some(Tag::End { name }));
            } else {
                self.at += 1;
                return self.start().map(Some);
            }
        }
    }
}

impl<'a> Iterator for Tags<'a> {
    type Item = Result<Tag<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let next = self.next_tag().transpose();
        self.failed = matches!(next, Some(Err(_)));
        next
    }
}

/// A value as it stands in a document: with the characters that mean
/// something to XML written as entities.
pub(crate) fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => out.push_str(&format!("&#{};", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A value as it was meant: its entities replaced by what they stand for.
/// An entity that is none stays as it is written.
pub(crate) fn unescape(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').filter(|end| *end <= 12);
        let replaced = end.and_then(|end| {
            let entity = &rest[1..end];
            let character = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let code = match entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => entity.strip_prefix('#').and_then(|decimal| decimal.parse().ok()),
                    };
                    code.and_then(char::from_u32)
                }
            };
            character.map(|character| (character, end + 1))
        });
        match replaced {
            Some((character, length)) => {
                out.push(character);
                rest = &rest[length..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replaces stretches of a document: `edits` are where and with what.
/// They must not overlap; their order does not matter.
pub(crate) fn replace(text: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by_key(|(at, _)| at.start);
    let mut out = String::with_capacity(text.len() + edits.iter().map(|(_, with)| with.len()).sum::<usize>());
    let mut from = 0;
    for (at, with) in edits {
        if at.start < from || at.end > text.len() {
            continue;
        }
        out.push_str(&text[from..at.start]);
        out.push_str(&with);
        from = at.end;
    }
    out.push_str(&text[from..]);
    out
}

/// A document as a tree: an element with its attributes, in the order
/// they were written, and its children.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Element>,
}

impl Element {
    /// Reads a document: its root element with everything in it.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        // The elements that are open, innermost last.
        let mut open: Vec<Element> = Vec::new();
        let mut count = 0usize;
        for tag in tags(text) {
            match tag? {
                Tag::Start { name, attributes, closed } => {
                    count += 1;
                    if count > MAX_ELEMENTS || open.len() >= MAX_DEPTH {
                        return Err("it is too large or too deeply nested to be a plugin's settings".to_string());
                    }
                    let element = Element {
                        name: name.to_string(),
                        attributes: attributes
                            .iter()
                            .map(|attribute| (attribute.name.to_string(), attribute.value()))
                            .collect(),
                        children: Vec::new(),
                    };
                    if !closed {
                        open.push(element);
                    } else if let Some(parent) = open.last_mut() {
                        parent.children.push(element);
                    } else {
                        return Ok(element);
                    }
                }
                Tag::End { name } => {
                    let element = open.pop().filter(|element| element.name == name);
                    let element = element.ok_or_else(|| format!("'{name}' ends where it was not open"))?;
                    match open.last_mut() {
                        Some(parent) => parent.children.push(element),
                        None => return Ok(element),
                    }
                }
            }
        }
        match open.last() {
            Some(element) => Err(format!("'{}' is not closed", element.name)),
            None => Err("it has no elements".to_string()),
        }
    }

    pub(crate) fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// Sets an attribute, in the place it has or after the others.
    pub(crate) fn set(&mut self, name: &str, value: &str) {
        match self.attributes.iter_mut().find(|(key, _)| key == name) {
            Some((_, old)) => *old = value.to_string(),
            None => self.attributes.push((name.to_string(), value.to_string())),
        }
    }

    pub(crate) fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    /// The document, written the way JUCE writes a plugin's state: a
    /// declaration, then the elements on one line.
    pub(crate) fn to_document(&self) -> String {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?> ");
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        // Not recursive: a tree read from a file may be as deep as the
        // file's writer liked.
        enum Step<'a> {
            Open(&'a Element),
            Close(&'a Element),
        }
        let mut steps = vec![Step::Open(self)];
        while let Some(step) = steps.pop() {
            match step {
                Step::Open(element) => {
                    out.push('<');
                    out.push_str(&element.name);
                    for (name, value) in &element.attributes {
                        out.push(' ');
                        out.push_str(name);
                        out.push_str("=\"");
                        out.push_str(&escape(value));
                        out.push('"');
                    }
                    if element.children.is_empty() {
                        out.push_str("/>");
                    } else {
                        out.push('>');
                        steps.push(Step::Close(element));
                        steps.extend(element.children.iter().rev().map(Step::Open));
                    }
                }
                Step::Close(element) => {
                    out.push_str("</");
                    out.push_str(&element.name);
                    out.push('>');
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- a state -->\n",
        "<STATE Name=\"Default\" Ver='4.1.0'>\n  <Path Value=\"C:\\Users\\Me &amp; You\"/>",
        "<Parameter Name=\"Gain\" Value=\"0.5\" Index=\"1\"/> text <Parameter Name = \"Mic\" Value = \"Dyn57\" />",
        "<Group><Parameter Name=\"Deep\" Value=\"&lt;1&gt;\"/></Group><![CDATA[<not a tag>]]></STATE>"
    );

    #[test]
    fn tags_are_found_with_where_their_values_are() {
        let found: Vec<Tag> = tags(DOCUMENT).map(Result::unwrap).collect();
        let names: Vec<String> = found
            .iter()
            .map(|tag| match tag {
                Tag::Start { name, closed: false, .. } => format!("<{name}>"),
                Tag::Start { name, closed: true, .. } => format!("<{name}/>"),
                Tag::End { name } => format!("</{name}>"),
            })
            .collect();
        assert_eq!(
            names,
            ["<STATE>", "<Path/>", "<Parameter/>", "<Parameter/>", "<Group>", "<Parameter/>", "</Group>", "</STATE>"]
        );
        let Tag::Start { attributes, .. } = &found[0] else { panic!() };
        assert_eq!((attributes[0].name, attributes[0].raw, attributes[1].raw), ("Name", "Default", "4.1.0"));
        let Tag::Start { attributes, .. } = &found[1] else { panic!() };
        assert_eq!(attributes[0].value(), "C:\\Users\\Me & You");
        // A value is where it says it is, spaces around the equals sign or
        // not.
        let Tag::Start { attributes, .. } = &found[3] else { panic!() };
        assert_eq!((attributes[1].name, &DOCUMENT[attributes[1].at.clone()]), ("Value", "Dyn57"));
    }

    #[test]
    fn values_are_changed_in_place_and_nothing_else_is() {
        let mut edits = Vec::new();
        for tag in tags(DOCUMENT) {
            if let Tag::Start { name: "Parameter", attributes, .. } = tag.unwrap() {
                let named = attributes.iter().find(|attribute| attribute.name == "Name").unwrap().value();
                let value = attributes.iter().find(|attribute| attribute.name == "Value").unwrap();
                match named.as_str() {
                    "Gain" => edits.push((value.at.clone(), escape("0.25"))),
                    "Deep" => edits.push((value.at.clone(), escape("a \"b\" & c"))),
                    _ => {}
                }
            }
        }
        edits.reverse();
        let edited = replace(DOCUMENT, edits);
        let expected =
            DOCUMENT.replace("Value=\"0.5\"", "Value=\"0.25\"").replace("&lt;1&gt;", "a &quot;b&quot; &amp; c");
        assert_eq!(edited, expected);
        assert_eq!(replace("abc", vec![(1..2, "XY".into()), (9..12, "no".into())]), "aXYc");
    }

    #[test]
    fn entities_are_what_they_stand_for() {
        assert_eq!(unescape("a &amp;&lt;&gt;&quot;&apos; b"), "a &<>\"' b");
        assert_eq!(unescape("&#65;&#x42;&#X43;"), "ABC");
        assert_eq!(unescape("fish & chips &nonsense; &#xZZ; &"), "fish & chips &nonsense; &#xZZ; &");
        assert_eq!(escape("a<b>&\"c\"\n"), "a&lt;b&gt;&amp;&quot;c&quot;&#10;");
        assert_eq!(unescape(&escape("tab\there & \"there\" <é>")), "tab\there & \"there\" <é>");
    }

    #[test]
    fn a_document_is_read_into_a_tree_and_written_back() {
        let tree = Element::parse(DOCUMENT).unwrap();
        assert_eq!((tree.name.as_str(), tree.attribute("Ver"), tree.children.len()), ("STATE", Some("4.1.0"), 4));
        assert_eq!(tree.child("Group").unwrap().children[0].attribute("Value"), Some("<1>"));
        assert_eq!(tree.child("Nothing"), None);

        let mut changed = tree.clone();
        changed.set("Name", "Lead & Rhythm");
        changed.set("New", "1");
        let written = changed.to_document();
        assert!(written.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?> <STATE Name=\"Lead &amp; Rhythm\" Ver=\"4.1.0\" New=\"1\"><Path "));
        assert!(
            written.ends_with("<Group><Parameter Name=\"Deep\" Value=\"&lt;1&gt;\"/></Group></STATE>"),
            "{written}"
        );
        assert_eq!(Element::parse(&written).unwrap(), changed);
        // Depth costs no stack.
        let deep = format!("{}{}", "<a>".repeat(200), "</a>".repeat(200));
        assert_eq!(Element::parse(&deep).unwrap().to_document().len(), deep.len() + 39 - 3);
    }

    #[test]
    fn what_is_not_well_formed_says_so() {
        let wrong = |text: &str| Element::parse(text).unwrap_err();
        assert_eq!(wrong(""), "it has no elements");
        assert_eq!(wrong("just text"), "it has no elements");
        assert_eq!(wrong("<a><b></a>"), "'a' ends where it was not open");
        assert_eq!(wrong("<a><b/>"), "'a' is not closed");
        assert_eq!(wrong("<a b></a>"), "the attribute 'b' of 'a' has no value");
        assert_eq!(wrong("<a b=c></a>"), "the value of 'b' of 'a' is not quoted");
        assert_eq!(wrong("<a b=\"c></a>"), "the value of 'b' is not closed");
        assert_eq!(wrong("<a"), "the tag 'a' is not closed");
        assert_eq!(wrong("<>"), "a tag has no name");
        assert_eq!(wrong("<!-- <a/>"), "a comment is not closed");
        assert_eq!(wrong(&"<a>".repeat(100_000)), "it is too large or too deeply nested to be a plugin's settings");
        // The walk stops at the first thing that is wrong.
        assert_eq!(tags("<a/><b c></b><d/>").count(), 2);
    }
}
