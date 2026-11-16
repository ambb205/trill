//! Looking at a plugin's state from outside: what kind of thing it is.
//!
//! Before a vendor's presets can be read, someone has to find out what the
//! plugin's state is: text, a document, a container that is known from
//! elsewhere, or bytes that give nothing away. This says which, in a
//! sentence, for the `inspect` example to print. It recognises the forms
//! the readers in this module were written for, and the common ones
//! besides.

use super::juce::{self, Envelope};
use super::xml::{self, Tag};
use super::{cbor, xfer};
use crate::vst3::state;

/// How many of its first lines a text is shown by, and how long a line
/// may be.
const SHOWN_LINES: usize = 3;
const SHOWN_WIDTH: usize = 72;
/// The least text before a NUL that makes a chunk a chunk of text.
const TEXT_CHUNK: usize = 64;

/// The two halves of a state that a VST3 device saved (its processor's
/// and its edit controller's); `None` for a state that is not one, such
/// as a CLAP plugin's, which is a single stretch of bytes.
pub fn split_state(saved: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let (_, state) = state::decode(saved).ok()?;
    Some((state.component, state.controller))
}

/// What `bytes`, a plugin's state or half of one, are: in a sentence or
/// two, with what it would take to read them.
pub fn describe_state(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "nothing: the plugin gives no state here".to_string();
    }
    let known = xfer_container(bytes)
        .or_else(|| enveloped(bytes))
        .or_else(|| bank(bytes))
        .or_else(|| wrapped(bytes))
        .or_else(|| document(bytes, "an XML document"))
        .or_else(|| tree(bytes))
        .or_else(|| packed(bytes))
        .or_else(|| text(bytes));
    known.unwrap_or_else(|| opaque(bytes))
}

fn shortened(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() > SHOWN_WIDTH {
        format!("{}…", line.chars().take(SHOWN_WIDTH).collect::<String>())
    } else {
        line.to_string()
    }
}

fn xfer_container(bytes: &[u8]) -> Option<String> {
    let (header, _) = xfer::header(bytes).ok()?;
    let fields: Vec<&str> = header.keys().map(String::as_str).collect();
    let about = format!("one of Xfer's containers: a JSON header ({})", fields.join(", "));
    Some(match xfer::unpack(bytes) {
        Ok(container) => {
            let entries = cbor::entries(&container.body).map(|entries| entries.len());
            match entries {
                Ok(count) => format!(
                    "{about}, then zstd-compressed CBOR that unpacks to {} bytes: a map of {count} entries",
                    container.body.len()
                ),
                Err(_) => format!(
                    "{about}, then a zstd-compressed body of {} bytes that is no CBOR map",
                    container.body.len()
                ),
            }
        }
        Err(reason) => format!("{about}, but {reason}"),
    })
}

/// What an XML text holds, as far as its tags say.
fn document(bytes: &[u8], what: &str) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.trim_start().starts_with('<') {
        return None;
    }
    let (mut root, mut attributes, mut elements, mut named) = (None, 0, 0usize, 0usize);
    for tag in xml::tags(text) {
        let Ok(Tag::Start { name, attributes: found, .. }) = tag else { continue };
        elements += 1;
        attributes += found.len();
        // Settings written one to an element, each with its name and value.
        let has = |wanted: &str| found.iter().any(|attribute| attribute.name.eq_ignore_ascii_case(wanted));
        named += usize::from(has("name") && has("value"));
        root.get_or_insert(name);
    }
    let root = root?;
    let settings = match named {
        0 => String::new(),
        named => format!(", {named} of them settings written as a name and a value"),
    };
    Some(format!("{what}: root <{root}>, {elements} elements with {attributes} attributes in all{settings}"))
}

fn enveloped(bytes: &[u8]) -> Option<String> {
    let envelope = Envelope::open(bytes)?;
    let inside = document(envelope.text.as_bytes(), "an XML document in JUCE's envelope (VC2!, a length, the text)")?;
    Some(match envelope.tail.len() {
        0 | 1 => inside,
        more => format!("{inside}; {more} bytes of the plugin format's wrapper follow it"),
    })
}

/// What JUCE's VST3 wrapper ends a state with, after the plugin's own
/// part: a small tree of its own (whether the plugin is bypassed), the
/// size of that tree in eight bytes, and these words.
const JUCE_PRIVATE: &[u8] = b"JUCEPrivateData";

/// The plugin's own part of a state that JUCE's wrapper added its own to.
fn before_wrapper(bytes: &[u8]) -> Option<&[u8]> {
    let rest = bytes.strip_suffix(JUCE_PRIVATE)?;
    let (rest, size) = rest.split_last_chunk::<8>()?;
    let size = usize::try_from(u64::from_le_bytes(*size)).ok()?;
    rest.get(..rest.len().checked_sub(size)?)
}

fn wrapped(bytes: &[u8]) -> Option<String> {
    let own = before_wrapper(bytes)?;
    let inside = if own.is_empty() { "nothing of the plugin's own".to_string() } else { describe_state(own) };
    Some(format!("{inside}; {} bytes that JUCE's wrapper keeps for itself follow it", bytes.len() - own.len()))
}

/// An old-style VST bank chunk, which wrapped VST2 plugins keep their
/// state in: `CcnK`, a size, `FBCh`, a version, the plugin's four letters.
fn bank(bytes: &[u8]) -> Option<String> {
    let at = bytes[..bytes.len().min(256)].windows(4).position(|window| window == b"CcnK")?;
    let kind = bytes.get(at + 8..at + 12)?;
    let plugin = String::from_utf8_lossy(bytes.get(at + 16..at + 20)?).into_owned();
    if kind != b"FBCh" && kind != b"FPCh" {
        return Some(format!(
            "an old-style VST bank of parameter values (plugin '{plugin}'), not a chunk of the plugin's own"
        ));
    }
    // A bank keeps 128 bytes for later before its chunk, a single program
    // 28 for its name.
    let size_at = at + if kind == b"FBCh" { 156 } else { 56 };
    let before = if at > 0 { format!(" behind a wrapper's header of {at} bytes") } else { String::new() };
    let size =
        bytes.get(size_at..size_at + 4).and_then(|size| Some(u32::from_be_bytes(size.try_into().ok()?) as usize));
    let chunk = size.and_then(|size| bytes.get(size_at + 4..(size_at + 4).checked_add(size)?));
    let Some(chunk) = chunk else {
        return Some(format!(
            "an old-style VST chunk (CcnK, plugin '{plugin}'){before}, cut short: it says it is longer than what is here"
        ));
    };
    // A chunk that is text ends at its first NUL, and what follows is the
    // plugin's own; a few readable bytes before a NUL are not that.
    let end = chunk.iter().position(|byte| *byte == 0).unwrap_or(chunk.len());
    let worded = || text(&chunk[..end]).filter(|_| end >= TEXT_CHUNK.min(chunk.len()));
    let inside =
        xfer_container(chunk).or_else(|| enveloped(chunk)).or_else(worded).unwrap_or_else(|| describe_state(chunk));
    Some(format!("an old-style VST chunk (CcnK, plugin '{plugin}'){before}, and in it {inside}"))
}

fn tree(bytes: &[u8]) -> Option<String> {
    let root = juce::tree(bytes).ok()?;
    let mut nodes = vec![&root];
    let (mut count, mut settings) = (0usize, 0usize);
    while let Some(node) = nodes.pop() {
        count += 1;
        settings += node.attributes.len();
        nodes.extend(&node.children);
    }
    (settings > 0).then(|| {
        format!("a tree in JUCE's binary form: root '{}', {count} nodes with {settings} named values", root.name)
    })
}

fn packed(bytes: &[u8]) -> Option<String> {
    let what = match bytes {
        [0x28, 0xb5, 0x2f, 0xfd, ..] => "zstd-compressed data",
        [0x1f, 0x8b, ..] => "gzip-compressed data",
        [0x78, 0x01 | 0x5e | 0x9c | 0xda, ..] => "zlib-compressed data",
        [b'P', b'K', 3, 4, ..] => "a zip archive",
        _ => return None,
    };
    Some(format!("{what}: what is in it shows once it is unpacked"))
}

fn text(bytes: &[u8]) -> Option<String> {
    let readable = bytes.iter().filter(|byte| matches!(byte, b'\t' | b'\n' | b'\r' | 0x20..=0x7e)).count();
    if readable * 100 < bytes.len() * 97 {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let lines = text.lines().filter(|line| !line.trim().is_empty());
    let shown: Vec<String> = lines.clone().take(SHOWN_LINES).map(shortened).collect();
    let kind = if text.trim_start().starts_with(['{', '[']) { "text that looks like JSON" } else { "text" };
    Some(format!("{kind}, {} lines; it begins: {}", lines.count(), shown.join(" | ")))
}

/// How evenly the values of the bytes are spread, in bits a byte: near 8
/// for what is compressed or encrypted, well under for tables of numbers.
fn entropy(bytes: &[u8]) -> f64 {
    let mut counts = [0usize; 256];
    for byte in bytes {
        counts[usize::from(*byte)] += 1;
    }
    let total = bytes.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| *count as f64 / total)
        .map(|share| -share * share.log2())
        .sum()
}

/// The first few stretches of readable characters in bytes that are
/// otherwise not text.
fn strings(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    for run in bytes.split(|byte| !matches!(byte, 0x20..=0x7e)) {
        if run.len() >= 5 && found.len() < 6 {
            found.push(shortened(&String::from_utf8_lossy(run)));
        }
    }
    found
}

fn opaque(bytes: &[u8]) -> String {
    // (Bytes that are all the same come to minus nothing.)
    let entropy = entropy(bytes).abs();
    let kind = match entropy {
        e if e > 7.5 => "compressed or encrypted by the look of it",
        e if e < 4.0 => "tables of numbers by the look of it",
        _ => "a format of the plugin's own",
    };
    let names = strings(bytes);
    let names = if names.is_empty() {
        "no readable names in it".to_string()
    } else {
        format!("readable in it: {}", names.join(", "))
    };
    format!("opaque: {} bytes, {kind} ({entropy:.1} bits a byte); {names}", bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor::{ample, archetype, serum, superior};
    use crate::vst3::class_id::ClassId;
    use crate::vst3::state::PluginState;

    #[test]
    fn a_state_is_said_to_be_what_it_is() {
        let said = |bytes: &[u8]| describe_state(bytes);
        assert_eq!(said(&[]), "nothing: the plugin gives no state here");

        // The forms the readers are for.
        let container = said(&serum::preset_file("Warm", "Me", 5.0, &[("Osc", &[0x01])]));
        assert!(container.starts_with("one of Xfer's containers: a JSON header (fileType, hash, "), "{container}");
        assert!(
            container.ends_with("then zstd-compressed CBOR that unpacks to 42 bytes: a map of 3 entries"),
            "{container}"
        );
        let tune = std::path::Path::new("tune");
        let guitar =
            ample::state(&ample::document("Default", "4.1.0", "AMHBg", tune, &[("Gain", "1"), ("Mic", "Dyn57")]));
        assert_eq!(
            said(&guitar.component),
            "an XML document in JUCE's envelope (VC2!, a length, the text): root <AMG_STATE>, 6 elements with 15 attributes \
             in all, 2 of them settings written as a name and a value; 29 bytes of the plugin format's wrapper follow it"
        );
        let drums = said(&superior::state("Kick1").component);
        assert!(drums.starts_with("an old-style VST chunk (CcnK, plugin 'SD3a') behind a wrapper's header of 16 bytes, and in it text, 22 lines; it begins: PlugVersion 3.4.4 | SAMPLER { | HQ {"), "{drums}");
        let amplifier = said(&archetype::preset_file("Lead", "1", &[("gate", "-80")]));
        assert_eq!(amplifier, "a tree in JUCE's binary form: root 'amp-x', 9 nodes with 6 named values");

        // What plugin wrappers put around a state is seen through: a
        // bank's chunk that is a document in an envelope, and JUCE's own
        // words after a state of text.
        let enveloped = Envelope { text: "", tail: &[] }.with("<reverb mix=\"1\" decay=\"0.5\"/>");
        let chunk = [&[0u8; 128][..], &(enveloped.len() as u32).to_be_bytes(), &enveloped].concat();
        let bank = [&b"CcnK\0\0\0\0FBCh\0\0\0\x02Ruum\0\0\0\x01\0\0\0\x01"[..], &chunk].concat();
        assert_eq!(
            said(&bank),
            "an old-style VST chunk (CcnK, plugin 'Ruum'), and in it an XML document in JUCE's envelope (VC2!, a length, \
             the text): root <reverb>, 1 elements with 2 attributes in all"
        );
        let private = b"JUCEPrivateData\0\x01\x01Bypass\0\x01\x01\x03\0";
        let size = (private.len() as u64).to_le_bytes();
        let wrapped = [&b"{\"format\": \"synth\", \"params\": {}}"[..], private, &size, b"JUCEPrivateData"].concat();
        assert_eq!(
            said(&wrapped),
            "text that looks like JSON, 1 lines; it begins: {\"format\": \"synth\", \"params\": {}}; 52 bytes that JUCE's \
             wrapper keeps for itself follow it"
        );
        assert_eq!(
            said(&[&private[..], &size, b"JUCEPrivateData"].concat()),
            "nothing of the plugin's own; 52 bytes that JUCE's wrapper keeps for itself follow it"
        );
        assert_eq!(
            said(&bank[..bank.len() - 8]),
            "an old-style VST chunk (CcnK, plugin 'Ruum'), cut short: it says it is longer than what is here"
        );
        assert!(said(&[0u8; 9]).starts_with("opaque: 9 bytes, tables of numbers by the look of it (0.0 bits"));

        // And what else there is.
        assert_eq!(
            said(b"<state a=\"1\"><knob value=\"2\"/></state>"),
            "an XML document: root <state>, 2 elements with 2 attributes in all"
        );
        assert_eq!(said(b"{\"gain\": 0.5}\n"), "text that looks like JSON, 1 lines; it begins: {\"gain\": 0.5}");
        assert_eq!(
            said(&[0x28, 0xb5, 0x2f, 0xfd, 1, 2, 3]),
            "zstd-compressed data: what is in it shows once it is unpacked"
        );
        let numbers: Vec<u8> = (0..400u32).flat_map(|n| ((n % 4) as f32).to_le_bytes()).collect();
        assert!(
            said(&numbers).starts_with("opaque: 1600 bytes, tables of numbers by the look of it"),
            "{}",
            said(&numbers)
        );
        let noise: Vec<u8> = (0..4000u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
        assert!(said(&noise).contains("compressed or encrypted by the look of it"), "{}", said(&noise));
        let named = [&[0u8, 1, 2, 250, 251][..], b"MasterVolume", &[0, 0, 0, 200, 3], b"abc", &[0, 0, 9, 9]].concat();
        assert!(said(&named).ends_with("readable in it: MasterVolume"), "{}", said(&named));
    }

    #[test]
    fn a_saved_vst3_state_splits_into_its_halves() {
        let class = ClassId::parse(serum::CLASS).unwrap();
        let halves = PluginState { component: b"processor".to_vec(), controller: b"controller".to_vec() };
        assert_eq!(split_state(&state::encode(class, &halves)), Some((b"processor".to_vec(), b"controller".to_vec())));
        assert_eq!(split_state(b"a CLAP plugin's state"), None);
        assert_eq!(split_state(&[]), None);
    }
}
