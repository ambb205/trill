//! Xfer Records' container, which Serum 2 keeps its preset files and both
//! halves of its state in:
//!
//! ```text
//! "XferJson\0"   length of the header: u64   header: JSON
//! size of the body: u32   kind: u32   body: zstd-compressed CBOR
//! ```
//!
//! Numbers are little-endian. The header says what the container holds
//! (product, version, a preset's name and author) and carries the MD5 of
//! the compressed body.

use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use serde_json::{Map, Value};
use std::io::Read;

pub(crate) const MAGIC: &[u8; 9] = b"XferJson\0";
/// The only kind of body there is: zstd-compressed CBOR.
pub(crate) const KIND_CBOR: u32 = 2;
/// The longest header read. Serum's are a few hundred bytes.
const MAX_HEADER: usize = 64 * 1024;
/// The largest body a container may unpack to. A patch with every
/// wavetable and sample in it is a few megabytes.
const MAX_BODY: usize = 256 * 1024 * 1024;
/// Magic word, header length, and after the header the body's size and
/// kind.
const PREFIX: usize = MAGIC.len() + 8;

/// A container taken apart.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Container {
    pub header: Map<String, Value>,
    pub kind: u32,
    /// The body, uncompressed.
    pub body: Vec<u8>,
}

/// Whether `bytes` begin the way a container does.
pub(crate) fn is_container(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// The header of the container `bytes` begin, and where the rest of it
/// starts. `bytes` need not hold the whole container: the first
/// [`header_room`] bytes of a file are enough for any header there is.
pub(crate) fn header(bytes: &[u8]) -> Result<(Map<String, Value>, usize), String> {
    if !is_container(bytes) {
        return Err("it is not one of Xfer's files".to_string());
    }
    let length = bytes.get(MAGIC.len()..PREFIX).ok_or("it is cut short")?;
    let length = u64::from_le_bytes(length.try_into().expect("eight bytes"));
    let length = usize::try_from(length).ok().filter(|length| *length <= MAX_HEADER).ok_or("its header is too long")?;
    let text = bytes.get(PREFIX..PREFIX + length).ok_or("it is cut short")?;
    match serde_json::from_slice(text) {
        Ok(Value::Object(header)) => Ok((header, PREFIX + length)),
        Ok(_) => Err("its header is not a list of fields".to_string()),
        Err(error) => Err(format!("its header cannot be read ({error})")),
    }
}

/// How much of a file to read to be sure of having its header.
pub(crate) const fn header_room() -> usize {
    PREFIX + MAX_HEADER
}

/// Takes a container apart. An error says what is wrong with the bytes,
/// as the end of a sentence about them.
pub(crate) fn unpack(bytes: &[u8]) -> Result<Container, String> {
    let (header, after) = header(bytes)?;
    let rest = bytes.get(after..).unwrap_or_default();
    let (size, kind) = match rest.get(..8) {
        Some(field) => (
            u32::from_le_bytes(field[..4].try_into().expect("four bytes")),
            u32::from_le_bytes(field[4..].try_into().expect("four bytes")),
        ),
        None => return Err("it is cut short".to_string()),
    };
    if kind != KIND_CBOR {
        return Err(format!("it holds its settings in a form this reader does not know (kind {kind})"));
    }
    let size = usize::try_from(size).ok().filter(|size| *size <= MAX_BODY).ok_or("its settings are too large")?;
    // One byte more than was promised is asked for: a body that goes on
    // past its size is not read to its end to find that out.
    let mut body = Vec::new();
    let decoder =
        StreamingDecoder::new(&rest[8..]).map_err(|error| format!("its settings cannot be unpacked ({error})"))?;
    decoder
        .take(size as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("its settings cannot be unpacked ({error})"))?;
    if body.len() != size {
        return Err("its settings are not the size it says".to_string());
    }
    Ok(Container { header, kind, body })
}

/// Puts a container together. The header's `hash` field is filled in with
/// the MD5 of the compressed body, which is what the plugin puts there.
pub(crate) fn pack(mut header: Map<String, Value>, body: &[u8]) -> Vec<u8> {
    let compressed = compress_to_vec(body, CompressionLevel::Fastest);
    header.insert("hash".to_string(), Value::String(format!("{:x}", md5::compute(&compressed))));
    // By name, as the plugin writes its headers, whatever order the map
    // keeps its fields in.
    let mut fields: Vec<(&String, &Value)> = header.iter().collect();
    fields.sort_by_key(|(name, _)| name.as_str());
    let fields: Vec<String> =
        fields.into_iter().map(|(name, value)| format!("{}:{value}", Value::String(name.clone()))).collect();
    let text = format!("{{{}}}", fields.join(",")).into_bytes();
    let mut out = Vec::with_capacity(PREFIX + text.len() + 8 + compressed.len());
    out.extend(MAGIC);
    out.extend((text.len() as u64).to_le_bytes());
    out.extend(text);
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(KIND_CBOR.to_le_bytes());
    out.extend(compressed);
    out
}

/// A text field of a header; empty if the header has none of that name.
pub(crate) fn field<'a>(header: &'a Map<String, Value>, name: &str) -> &'a str {
    header.get(name).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header_of(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn a_container_comes_apart_into_what_it_was_made_of() {
        // A body that compresses, and one that does not.
        let noise: Vec<u8> = (0..5000u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for body in [vec![7u8; 100_000], noise, Vec::new()] {
            let packed = pack(header_of(json!({ "product": "Serum2", "version": 5.0 })), &body);
            assert!(is_container(&packed));
            let container = unpack(&packed).unwrap();
            assert_eq!((container.kind, &container.body), (KIND_CBOR, &body));
            assert_eq!(field(&container.header, "product"), "Serum2");
            assert_eq!(container.header["version"], json!(5.0));
            // The hash is of the body as it lies in the container.
            let (_, after) = header(&packed).unwrap();
            assert_eq!(field(&container.header, "hash"), format!("{:x}", md5::compute(&packed[after + 8..])));
            assert_eq!(field(&container.header, "hash").len(), 32);
        }
        // The header's fields are in the order of their names, and a whole
        // number that was written with a decimal point keeps it.
        let packed = pack(header_of(json!({ "version": 5.0, "product": "Serum2", "component": "processor" })), &[]);
        let (_, after) = header(&packed).unwrap();
        let text = String::from_utf8_lossy(&packed[PREFIX..after]).into_owned();
        assert!(text.starts_with(r#"{"component":"processor","hash":""#), "{text}");
        assert!(text.ends_with(r#"","product":"Serum2","version":5.0}"#), "{text}");
        assert!(pack(Map::new(), &vec![7u8; 100_000]).len() < 2000, "a body of one byte over and over is packed small");
    }

    #[test]
    fn a_header_is_read_from_the_start_of_a_file_alone() {
        let packed = pack(header_of(json!({ "presetName": "Warm Room", "tags": ["Pad"] })), &[1, 2, 3]);
        let (read, after) = header(&packed[..after_header(&packed)]).unwrap();
        assert_eq!(field(&read, "presetName"), "Warm Room");
        assert_eq!(field(&read, "presetAuthor"), "", "a field that is not there is empty");
        assert_eq!(after, after_header(&packed));
    }

    fn after_header(packed: &[u8]) -> usize {
        PREFIX + u64::from_le_bytes(packed[MAGIC.len()..PREFIX].try_into().unwrap()) as usize
    }

    #[test]
    fn what_is_not_a_container_says_what_is_wrong_with_it() {
        let good = pack(header_of(json!({ "product": "Serum2" })), &[1, 2, 3, 4]);
        let after = after_header(&good);
        let wrong = |bytes: &[u8]| unpack(bytes).unwrap_err();
        assert_eq!(wrong(b"RIFF....WAVE"), "it is not one of Xfer's files");
        assert_eq!(wrong(&good[..12]), "it is cut short");
        assert_eq!(wrong(&good[..after + 3]), "it is cut short");
        assert!(wrong(&good[..good.len() - 2]).starts_with("its settings"), "{}", wrong(&good[..good.len() - 2]));

        // A header that claims to be enormous, and one that is no object.
        let mut long = good.clone();
        long[MAGIC.len()..PREFIX].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(wrong(&long), "its header is too long");
        let mut list = MAGIC.to_vec();
        list.extend(2u64.to_le_bytes());
        list.extend(b"[]");
        assert_eq!(wrong(&list), "its header is not a list of fields");

        // Another kind of body, a size that is a lie, and a body that is
        // not zstd at all.
        let mut kind = good.clone();
        kind[after + 4..after + 8].copy_from_slice(&9u32.to_le_bytes());
        assert_eq!(wrong(&kind), "it holds its settings in a form this reader does not know (kind 9)");
        for size in [3u32, 5, u32::MAX] {
            let mut sized = good.clone();
            sized[after..after + 4].copy_from_slice(&size.to_le_bytes());
            let said = wrong(&sized);
            assert!(said == "its settings are not the size it says" || said == "its settings are too large", "{said}");
        }
        let mut garbage = good[..after + 8].to_vec();
        garbage.extend([0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3]);
        assert!(wrong(&garbage).starts_with("its settings cannot be unpacked"), "{}", wrong(&garbage));
    }
}
