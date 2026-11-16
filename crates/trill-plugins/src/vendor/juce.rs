//! What plugins built with JUCE have in common, as far as their states
//! and preset files go.
//!
//! Such a plugin's state very often is an XML document in JUCE's
//! envelope: the bytes `VC2!`, the document's length as a little-endian
//! u32, the document, a zero byte; and after that whatever the plugin
//! format's wrapper adds. [`Envelope`] takes that apart and puts it
//! together again around another document.
//!
//! The other thing JUCE gives a plugin is a tree of named values, which
//! some write to their preset files in its binary form: for each node its
//! name, its properties (a name and a typed value each) and its children.
//! [`tree`] reads one into the same [`Element`] an XML document is read
//! into, which is what the plugin itself makes of it when it saves its
//! state.

use super::xml::Element;

const MAGIC: &[u8; 4] = b"VC2!";
/// How deep a tree may nest, and how many nodes and properties it may
/// have: far beyond any preset, well short of what would hurt.
const MAX_DEPTH: usize = 128;
const MAX_ITEMS: usize = 1_000_000;

/// A state that is an XML document in JUCE's envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Envelope<'a> {
    /// The document.
    pub text: &'a str,
    /// What follows the document, the zero that ends it included.
    pub tail: &'a [u8],
}

impl<'a> Envelope<'a> {
    /// Takes a state apart, if it is one of these.
    pub(crate) fn open(state: &'a [u8]) -> Option<Self> {
        let length = state.strip_prefix(MAGIC)?.get(..4)?;
        let length = u32::from_le_bytes(length.try_into().ok()?) as usize;
        let text = state.get(8..8usize.checked_add(length)?)?;
        let text = std::str::from_utf8(text).ok()?;
        Some(Self { text, tail: &state[8 + length..] })
    }

    /// The state with `text` in place of its document.
    pub(crate) fn with(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + text.len() + self.tail.len());
        out.extend(MAGIC);
        out.extend((text.len() as u32).to_le_bytes());
        out.extend(text.as_bytes());
        out.extend(self.tail);
        out
    }
}

/// Reads through the bytes of a tree.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    items: usize,
}

impl<'a> Reader<'a> {
    fn cut() -> String {
        "it ends in the middle of its settings".to_string()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(count).filter(|end| *end <= self.bytes.len()).ok_or_else(Self::cut)?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    /// A string that ends with a zero byte.
    fn string(&mut self) -> Result<&'a str, String> {
        let rest = &self.bytes[self.at..];
        let length = rest.iter().position(|byte| *byte == 0).ok_or_else(Self::cut)?;
        let text = std::str::from_utf8(&rest[..length]).map_err(|_| "it has a name that is not text".to_string())?;
        self.at += length + 1;
        Ok(text)
    }

    /// A whole number as JUCE packs it: how many bytes, then the bytes,
    /// lowest first. Counts and sizes are never negative.
    fn count(&mut self) -> Result<usize, String> {
        let width = self.take(1)?[0];
        if width & 0x80 != 0 || width > 4 {
            return Err("it has a count that is no count".to_string());
        }
        let bytes = self.take(usize::from(width))?;
        Ok(bytes.iter().rev().fold(0usize, |value, byte| value << 8 | usize::from(*byte)))
    }

    /// A value, as the text a plugin writes it as into its state.
    fn value(&mut self) -> Result<String, String> {
        let size = self.count()?;
        if size == 0 {
            return Ok(String::new());
        }
        let body = self.take(size)?;
        let (kind, data) = (body[0], &body[1..]);
        let number = |width: usize| data.get(..width).ok_or_else(Self::cut);
        Ok(match kind {
            1 => i32::from_le_bytes(number(4)?.try_into().expect("four bytes")).to_string(),
            2 => "1".to_string(),
            3 => "0".to_string(),
            4 => f64::from_le_bytes(number(8)?.try_into().expect("eight bytes")).to_string(),
            5 => {
                let text = data.split(|byte| *byte == 0).next().unwrap_or_default();
                String::from_utf8_lossy(text).into_owned()
            }
            6 => i64::from_le_bytes(number(8)?.try_into().expect("eight bytes")).to_string(),
            // Arrays and raw bytes have no place in an attribute.
            _ => String::new(),
        })
    }

    fn counted(&mut self) -> Result<(), String> {
        self.items += 1;
        if self.items > MAX_ITEMS {
            return Err("it is too large to be a preset".to_string());
        }
        Ok(())
    }

    fn node(&mut self, depth: usize) -> Result<Element, String> {
        if depth > MAX_DEPTH {
            return Err(format!("it nests more than {MAX_DEPTH} deep"));
        }
        self.counted()?;
        let name = self.string()?.to_string();
        let mut element = Element { name, ..Element::default() };
        for _ in 0..self.count()? {
            self.counted()?;
            let name = self.string()?.to_string();
            let value = self.value()?;
            element.attributes.push((name, value));
        }
        for _ in 0..self.count()? {
            let child = self.node(depth + 1)?;
            element.children.push(child);
        }
        Ok(element)
    }
}

/// Reads a tree that was written in JUCE's binary form. An error says
/// what is wrong with the bytes, as the end of a sentence about them.
pub(crate) fn tree(bytes: &[u8]) -> Result<Element, String> {
    let mut reader = Reader { bytes, at: 0, items: 0 };
    let root = reader.node(0)?;
    if root.name.is_empty() || !root.name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')) {
        return Err("it is not a tree of settings".to_string());
    }
    Ok(root)
}

/// A tree in JUCE's binary form, every value a string: what a test makes
/// its preset files of.
#[cfg(test)]
pub(crate) fn tree_bytes(element: &Element) -> Vec<u8> {
    fn count(out: &mut Vec<u8>, value: usize) {
        let bytes = (value as u32).to_le_bytes();
        let width = bytes.iter().rposition(|byte| *byte != 0).map_or(0, |last| last + 1);
        out.push(width as u8);
        out.extend(&bytes[..width]);
    }
    let mut out = Vec::new();
    out.extend(element.name.as_bytes());
    out.push(0);
    count(&mut out, element.attributes.len());
    for (name, value) in &element.attributes {
        out.extend(name.as_bytes());
        out.push(0);
        count(&mut out, value.len() + 2);
        out.push(5);
        out.extend(value.as_bytes());
        out.push(0);
    }
    count(&mut out, element.children.len());
    for child in &element.children {
        out.extend(tree_bytes(child));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(name: &str, attributes: &[(&str, &str)], children: Vec<Element>) -> Element {
        Element {
            name: name.into(),
            attributes: attributes.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect(),
            children,
        }
    }

    #[test]
    fn a_state_in_an_envelope_comes_apart_and_goes_together_again() {
        let document = "<?xml version=\"1.0\" encoding=\"UTF-8\"?> <STATE Name=\"Default\"/>";
        let wrapper = b"\0\0\0\0\0\0\0\0JUCEPrivateData\0\x01\x01Bypass\0";
        let mut state = MAGIC.to_vec();
        state.extend((document.len() as u32).to_le_bytes());
        state.extend(document.as_bytes());
        state.push(0);
        state.extend(wrapper);

        let envelope = Envelope::open(&state).unwrap();
        assert_eq!(envelope.text, document);
        assert_eq!(envelope.tail.len(), 1 + wrapper.len());
        assert_eq!(envelope.with(document), state, "unchanged, it is the state it was");
        let longer = envelope.with("<STATE Name=\"Another one, longer than the first\"/>");
        let again = Envelope::open(&longer).unwrap();
        assert_eq!((again.text, again.tail), ("<STATE Name=\"Another one, longer than the first\"/>", envelope.tail));

        // What is no envelope: other bytes, one cut short, a length that
        // is a lie, and a document that is not text.
        assert_eq!(Envelope::open(b"XferJson\0"), None);
        assert_eq!(Envelope::open(&state[..6]), None);
        assert_eq!(Envelope::open(&state[..20]), None);
        let mut lying = state.clone();
        lying[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Envelope::open(&lying), None);
        let mut binary = state.clone();
        binary[10] = 0xff;
        assert_eq!(Envelope::open(&binary), None);
    }

    #[test]
    fn a_tree_is_read_into_the_document_the_plugin_would_write() {
        let root = element(
            "amp-x",
            &[("version", "0.4.0"), ("name", "Default")],
            vec![element(
                "appModel",
                &[("selectedAmp", "2")],
                vec![element(
                    "subModels",
                    &[],
                    vec![element("parameters", &[("gateThreshold", "-80"), ("note", "")], vec![])],
                )],
            )],
        );
        let bytes = tree_bytes(&root);
        // As the format has it: a name, a count, and for each property a
        // name and a sized, typed value.
        assert!(bytes.starts_with(b"amp-x\0\x01\x02version\0\x01\x07\x050.4.0\0name\0"));
        assert_eq!(tree(&bytes).unwrap(), root);

        // Values of the other kinds, as the text a state has them as.
        let mut typed = b"n\0\x01\x06".to_vec();
        typed.extend(b"i\0\x01\x05\x01\x2a\0\0\0");
        typed.extend(b"t\0\x01\x01\x02");
        typed.extend(b"f\0\x01\x01\x03");
        typed.extend(b"d\0\x01\x09\x04");
        typed.extend(0.5f64.to_le_bytes());
        typed.extend(b"l\0\x01\x09\x06");
        typed.extend((-7i64).to_le_bytes());
        typed.extend(b"v\0\0");
        typed.push(0);
        let read = tree(&typed).unwrap();
        let values: Vec<&str> = read.attributes.iter().map(|(_, value)| value.as_str()).collect();
        assert_eq!(values, ["42", "1", "0", "0.5", "-7", ""]);
    }

    #[test]
    fn bytes_that_are_no_tree_say_what_is_wrong_and_cost_nothing() {
        let good = tree_bytes(&element("a", &[("k", "v")], vec![element("b", &[], vec![])]));
        let wrong = |bytes: &[u8]| tree(bytes).unwrap_err();
        assert_eq!(wrong(&[]), "it ends in the middle of its settings");
        assert_eq!(wrong(&good[..good.len() - 1]), "it ends in the middle of its settings");
        assert_eq!(wrong(b"<?xml version=\"1.0\"?>\0\0\0"), "it is not a tree of settings");
        assert_eq!(wrong(b"a\0\x85"), "it has a count that is no count");
        // A count of millions with nothing behind it, and children without
        // end.
        assert_eq!(wrong(b"a\0\x04\xff\xff\xff\x7f"), "it ends in the middle of its settings");
        let mut deep = Vec::new();
        for _ in 0..10_000 {
            deep.extend(b"a\0\0\x01\x01");
        }
        assert_eq!(wrong(&deep), "it nests more than 128 deep");
        assert_eq!(wrong(b"a\xff\0\0\0"), "it has a name that is not text");
    }
}
