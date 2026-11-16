//! As much of CBOR (RFC 8949) as it takes to rearrange a map: finding
//! where each of its entries begins and ends, and writing a map out of
//! entries that are already encoded.
//!
//! Nothing is decoded into values. A patch is a map of several hundred
//! entries, some of them megabytes of wavetable, and all that is wanted is
//! the same map with one entry taken out and one put in. Leaving every
//! other entry as the bytes it was keeps it exactly what the plugin wrote.

/// How deep arrays, maps and tags may nest. A patch nests five or six
/// deep; the limit keeps a file made of nothing but opening brackets from
/// using up the stack.
const MAX_DEPTH: usize = 64;

/// One entry of a map: its key and its value, each as it is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
    /// The key, if it is a text string of one piece.
    pub name: Option<&'a str>,
}

/// The head of a data item: its major type, the number that follows it
/// (`None` for an item of indefinite length), and where its content starts.
struct Head {
    major: u8,
    argument: Option<u64>,
    after: usize,
}

fn head(bytes: &[u8], at: usize) -> Result<Head, String> {
    let cut = || "it ends in the middle of an item".to_string();
    let first = *bytes.get(at).ok_or_else(cut)?;
    let (major, info) = (first >> 5, first & 0x1f);
    let wide = |width: usize| -> Result<u64, String> {
        let field = bytes.get(at + 1..at + 1 + width).ok_or_else(cut)?;
        Ok(field.iter().fold(0u64, |value, byte| value << 8 | u64::from(*byte)))
    };
    let (argument, after) = match info {
        0..=23 => (Some(u64::from(info)), at + 1),
        24 => (Some(wide(1)?), at + 2),
        25 => (Some(wide(2)?), at + 3),
        26 => (Some(wide(4)?), at + 5),
        27 => (Some(wide(8)?), at + 9),
        31 if matches!(major, 2..=5 | 7) => (None, at + 1),
        _ => return Err(format!("it has an item that is not CBOR (0x{first:02x})")),
    };
    Ok(Head { major, argument, after })
}

/// Whether the byte at `at` is the "break" that ends an item of
/// indefinite length.
fn is_break(bytes: &[u8], at: usize) -> bool {
    bytes.get(at) == Some(&0xff)
}

/// Where the data item that starts at `at` ends.
fn skip(bytes: &[u8], at: usize, depth: usize) -> Result<usize, String> {
    if depth > MAX_DEPTH {
        return Err(format!("it nests more than {MAX_DEPTH} deep"));
    }
    let Head { major, argument, after } = head(bytes, at)?;
    let within = |end: Option<usize>| end.filter(|end| *end <= bytes.len()).ok_or("it ends in the middle of an item");
    let count = |argument: u64| usize::try_from(argument).map_err(|_| "it claims more than there is room for");
    match (major, argument) {
        // Numbers and simple values are all head.
        (0 | 1, _) => Ok(after),
        (7, Some(_)) => Ok(after),
        (7, None) => Err("it has a break where no item is open".to_string()),
        (2 | 3, Some(length)) => Ok(within(after.checked_add(count(length)?))?),
        // A string in pieces: each a string of definite length.
        (2 | 3, None) => {
            let mut at = after;
            while !is_break(bytes, at) {
                let piece = head(bytes, at)?;
                let length = piece.argument.filter(|_| piece.major == major).ok_or("it has a string in bad pieces")?;
                at = within(piece.after.checked_add(count(length)?))?;
            }
            Ok(at + 1)
        }
        (4 | 5, Some(length)) => {
            let items = count(length)?.checked_mul(if major == 5 { 2 } else { 1 }).ok_or("it claims too many items")?;
            // Every item takes a byte at least: a count beyond what is
            // left is refused before anything is walked.
            if items > bytes.len().saturating_sub(after) {
                return Err("it claims more items than it has bytes".to_string());
            }
            (0..items).try_fold(after, |at, _| skip(bytes, at, depth + 1))
        }
        (4 | 5, None) => {
            let mut at = after;
            while !is_break(bytes, at) {
                at = skip(bytes, at, depth + 1)?;
                if major == 5 {
                    at = skip(bytes, at, depth + 1)?;
                }
            }
            Ok(at + 1)
        }
        (6, _) => skip(bytes, after, depth + 1),
        _ => Err("it has an item that is not CBOR".to_string()),
    }
}

/// The entries of the map that `bytes` is, in the order they are in. An
/// error says what is wrong with the bytes, as the end of a sentence
/// about them.
pub(crate) fn entries(bytes: &[u8]) -> Result<Vec<Entry<'_>>, String> {
    let Head { major, argument, after } = head(bytes, 0)?;
    if major != 5 {
        return Err("it is not a map of settings".to_string());
    }
    let mut found = Vec::new();
    let mut at = after;
    let mut entry = |at: usize| -> Result<usize, String> {
        let key_end = skip(bytes, at, 1)?;
        let value_end = skip(bytes, key_end, 1)?;
        let key = &bytes[at..key_end];
        let name = head(key, 0).ok().and_then(|head| {
            let length = usize::try_from(head.argument?).ok()?;
            let text = key.get(head.after..head.after.checked_add(length)?)?;
            (head.major == 3).then(|| std::str::from_utf8(text).ok()).flatten()
        });
        found.push(Entry { key, value: &bytes[key_end..value_end], name });
        Ok(value_end)
    };
    match argument {
        Some(count) => {
            let count = usize::try_from(count).ok().filter(|count| *count <= bytes.len() / 2);
            for _ in 0..count.ok_or("it claims more entries than it has bytes")? {
                at = entry(at)?;
            }
        }
        None => {
            while !is_break(bytes, at) {
                at = entry(at)?;
            }
            at += 1;
        }
    }
    if at != bytes.len() {
        return Err("something follows its settings".to_string());
    }
    Ok(found)
}

/// The head of an item of `major` type with the number `argument`, in the
/// shortest form there is.
fn write_head(out: &mut Vec<u8>, major: u8, argument: u64) {
    let major = major << 5;
    match argument {
        0..=23 => out.push(major | argument as u8),
        24..=0xff => out.extend([major | 24, argument as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend((argument as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend((argument as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend(argument.to_be_bytes());
        }
    }
}

/// A text string, encoded.
pub(crate) fn text(value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 9);
    write_head(&mut out, 3, value.len() as u64);
    out.extend(value.as_bytes());
    out
}

/// A map of `entries`, each an encoded key and an encoded value, in the
/// order given.
pub(crate) fn map<'a>(entries: &[(&'a [u8], &'a [u8])]) -> Vec<u8> {
    let size: usize = entries.iter().map(|(key, value)| key.len() + value.len()).sum();
    let mut out = Vec::with_capacity(size + 9);
    write_head(&mut out, 5, entries.len() as u64);
    for (key, value) in entries {
        out.extend(*key);
        out.extend(*value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{"name": "Pad", "voices": 16, "table": h'00..' (300 bytes), "fx": [1, {"mix": 0.5}], "on": true}`
    fn patch() -> Vec<u8> {
        let mut bytes = vec![0xa5];
        bytes.extend(text("name"));
        bytes.extend(text("Pad"));
        bytes.extend(text("voices"));
        bytes.push(0x10);
        bytes.extend(text("table"));
        bytes.extend([0x59, 0x01, 0x2c]);
        bytes.extend(std::iter::repeat_n(7u8, 300));
        bytes.extend(text("fx"));
        bytes.extend([0x82, 0x01, 0xa1]);
        bytes.extend(text("mix"));
        bytes.extend([0xfb, 0x3f, 0xe0, 0, 0, 0, 0, 0, 0]);
        bytes.extend(text("on"));
        bytes.push(0xf5);
        bytes
    }

    #[test]
    fn a_maps_entries_are_found_without_decoding_what_is_in_them() {
        let bytes = patch();
        let found = entries(&bytes).unwrap();
        let names: Vec<&str> = found.iter().filter_map(|entry| entry.name).collect();
        assert_eq!(names, ["name", "voices", "table", "fx", "on"]);
        assert_eq!(found[0].value, text("Pad"));
        assert_eq!(found[1].value, [0x10]);
        assert_eq!(found[2].value.len(), 3 + 300);
        assert_eq!(found[3].value.len(), 3 + 4 + 9);
        assert_eq!(found[4].value, [0xf5]);

        // Put together again, entry by entry, it is the map it was.
        let pairs: Vec<(&[u8], &[u8])> = found.iter().map(|entry| (entry.key, entry.value)).collect();
        assert_eq!(map(&pairs), bytes);
        // One taken out and one put in.
        let (key, value) = (text("component"), text("processor"));
        let mut edited: Vec<(&[u8], &[u8])> = pairs.iter().copied().filter(|(key, _)| *key != text("on")).collect();
        edited.push((&key, &value));
        let again = map(&edited);
        let names: Vec<&str> = entries(&again).unwrap().iter().filter_map(|entry| entry.name).collect();
        assert_eq!(names, ["name", "voices", "table", "fx", "component"]);
    }

    #[test]
    fn maps_and_strings_of_indefinite_length_are_walked_to_their_break() {
        // {_ "a": [_ 1, 2], "b": (_ "x", "yz"), 3: h'' }
        let mut bytes = vec![0xbf];
        bytes.extend(text("a"));
        bytes.extend([0x9f, 0x01, 0x02, 0xff]);
        bytes.extend(text("b"));
        bytes.extend([0x7f, 0x61, b'x', 0x62, b'y', b'z', 0xff]);
        bytes.extend([0x03, 0x40]);
        bytes.push(0xff);
        let found = entries(&bytes).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!((found[0].name, found[1].name, found[2].name), (Some("a"), Some("b"), None));
        assert_eq!(found[1].value.len(), 7);
        // A tag is one item with the item it tags.
        let tagged = [0xa1, 0x61, b't', 0xc1, 0x1a, 0, 0, 0, 1];
        assert_eq!(entries(&tagged).unwrap()[0].value, [0xc1, 0x1a, 0, 0, 0, 1]);
    }

    #[test]
    fn heads_are_written_as_short_as_they_can_be() {
        assert_eq!(text(""), [0x60]);
        assert_eq!(text("a"), [0x61, b'a']);
        assert_eq!(text(&"a".repeat(24))[..2], [0x78, 24]);
        assert_eq!(text(&"a".repeat(300))[..3], [0x79, 0x01, 0x2c]);
        assert_eq!(text(&"a".repeat(70_000))[..5], [0x7a, 0, 1, 0x11, 0x70]);
        assert_eq!(map(&[]), [0xa0]);
        let many: Vec<(&[u8], &[u8])> = vec![(&[0x00][..], &[0x00][..]); 30];
        assert_eq!(map(&many)[..2], [0xb8, 30]);
    }

    #[test]
    fn bytes_that_are_not_a_map_say_what_is_wrong_and_cost_nothing() {
        let wrong = |bytes: &[u8]| entries(bytes).unwrap_err();
        assert_eq!(wrong(&[]), "it ends in the middle of an item");
        assert_eq!(wrong(&[0x83, 1, 2, 3]), "it is not a map of settings");
        assert_eq!(wrong(&patch()[..40]), "it ends in the middle of an item");
        let mut followed = patch();
        followed.push(0x00);
        assert_eq!(wrong(&followed), "something follows its settings");
        // Counts and lengths far beyond the bytes there are.
        assert_eq!(
            wrong(&[0xbb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            "it claims more entries than it has bytes"
        );
        assert_eq!(
            wrong(&[0xa1, 0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            "it ends in the middle of an item"
        );
        assert_eq!(wrong(&[0xa1, 0x00, 0x9a, 0xff, 0xff, 0xff, 0xff]), "it claims more items than it has bytes");
        // Brackets without end.
        let mut deep = vec![0xa1, 0x00];
        deep.extend(std::iter::repeat_n(0x81u8, 100_000));
        assert_eq!(wrong(&deep), "it nests more than 64 deep");
        assert_eq!(wrong(&[0xa1, 0x00, 0xff]), "it has a break where no item is open");
        assert_eq!(wrong(&[0xa1, 0x1c, 0x00]), "it has an item that is not CBOR (0x1c)");
        assert!(wrong(&[0xbf, 0x00, 0x00]).contains("ends in the middle"));
    }
}
