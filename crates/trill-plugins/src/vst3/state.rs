//! The bytes a VST3 device stores in the project file.
//!
//! A VST3 plugin has two states: its processor's ("component state") and
//! its edit controller's. A device needs both back, in one blob. Rather
//! than invent a container, the blob is a VST3 preset file, the format
//! the SDK defines for exactly this pair (`vstpresetfile.h`):
//!
//! ```text
//! "VST3"  version: i32  class id: 32 ASCII digits  list offset: i64
//! chunk data ...
//! "List"  count: i32  { id: 4 bytes, offset: i64, size: i64 } ...
//! ```
//!
//! All numbers are little-endian and offsets count from the start. The
//! component state is the chunk `Comp`, the controller state `Cont`. The
//! blob therefore says which plugin it belongs to, and written to a
//! `.vstpreset` file it loads in any other VST3 host.

use super::class_id::ClassId;
use std::io::{Read, Seek, SeekFrom};

const HEADER_ID: &[u8; 4] = b"VST3";
const LIST_ID: &[u8; 4] = b"List";
const COMPONENT_CHUNK: &[u8; 4] = b"Comp";
const CONTROLLER_CHUNK: &[u8; 4] = b"Cont";
/// A preset's meta information: a small XML document of attributes.
const INFO_CHUNK: &[u8; 4] = b"Info";
/// The most meta information read from a preset file, and the most chunks
/// looked through for it. Both are far beyond what a preset has.
const MAX_INFO: usize = 64 * 1024;
const MAX_CHUNKS: usize = 64;
/// The only version of the format there is.
const FORMAT_VERSION: i32 = 1;
/// Header id, version, class id and list offset.
const HEADER_SIZE: usize = 4 + 4 + 32 + 8;
/// Chunk id, offset and size.
const ENTRY_SIZE: usize = 4 + 8 + 8;

/// The two states of a plugin. An empty state is one the plugin does not
/// have.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PluginState {
    pub component: Vec<u8>,
    pub controller: Vec<u8>,
}

impl PluginState {
    /// Whether the plugin gave nothing worth storing.
    pub(crate) fn is_empty(&self) -> bool {
        self.component.is_empty() && self.controller.is_empty()
    }
}

/// Packs the state of a plugin of `class` into one blob.
pub(crate) fn encode(class: ClassId, state: &PluginState) -> Vec<u8> {
    let chunks: Vec<(&[u8; 4], &[u8])> = [(COMPONENT_CHUNK, &state.component), (CONTROLLER_CHUNK, &state.controller)]
        .into_iter()
        .filter(|(_, data)| !data.is_empty())
        .map(|(id, data)| (id, data.as_slice()))
        .collect();
    let data_size: usize = chunks.iter().map(|(_, data)| data.len()).sum();
    let list_offset = HEADER_SIZE + data_size;

    let mut bytes = Vec::with_capacity(list_offset + 8 + chunks.len() * ENTRY_SIZE);
    bytes.extend_from_slice(HEADER_ID);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(class.to_string().as_bytes());
    bytes.extend_from_slice(&(list_offset as i64).to_le_bytes());

    let mut entries = Vec::with_capacity(chunks.len());
    for (id, data) in &chunks {
        entries.push((*id, bytes.len(), data.len()));
        bytes.extend_from_slice(data);
    }
    bytes.extend_from_slice(LIST_ID);
    bytes.extend_from_slice(&(entries.len() as i32).to_le_bytes());
    for (id, offset, size) in entries {
        bytes.extend_from_slice(id);
        bytes.extend_from_slice(&(offset as i64).to_le_bytes());
        bytes.extend_from_slice(&(size as i64).to_le_bytes());
    }
    bytes
}

/// Unpacks a blob made by [`encode`], or any VST3 preset file. Chunks other
/// than the two states (a preset's meta information) are ignored.
///
/// The blob comes from a project file, which may be damaged or edited, so
/// every offset and size in it is checked against the blob.
pub(crate) fn decode(bytes: &[u8]) -> Result<(ClassId, PluginState), String> {
    let damaged = |what: &str| format!("the saved state is damaged ({what})");

    let header = bytes.get(..HEADER_SIZE).ok_or("the saved state is not a VST3 state (too short)")?;
    if &header[..4] != HEADER_ID {
        return Err("the saved state is not a VST3 state".to_string());
    }
    let version = i32::from_le_bytes(array(&header[4..8]));
    if version != FORMAT_VERSION {
        return Err(format!("the saved state has an unknown format version ({version})"));
    }
    let class = std::str::from_utf8(&header[8..40])
        .ok()
        .and_then(ClassId::parse)
        .ok_or_else(|| damaged("its class id is not a class id"))?;

    let list = offset(i64::from_le_bytes(array(&header[40..48]))).and_then(|start| bytes.get(start..));
    let list = list.ok_or_else(|| damaged("its chunk list lies outside it"))?;
    if list.get(..4) != Some(LIST_ID.as_slice()) {
        return Err(damaged("it has no chunk list"));
    }
    let count = list.get(4..8).map(|count| i32::from_le_bytes(array(count)));
    let count = count.and_then(|count| usize::try_from(count).ok()).ok_or_else(|| damaged("its chunk count"))?;
    // The count is checked against what is really there before anything is
    // read, so a huge one costs nothing.
    let entries = &list[8..];
    if count.checked_mul(ENTRY_SIZE).is_none_or(|size| size > entries.len()) {
        return Err(damaged("its chunk list is cut short"));
    }

    let mut state = PluginState::default();
    for entry in entries.chunks_exact(ENTRY_SIZE).take(count) {
        let start = offset(i64::from_le_bytes(array(&entry[4..12])));
        let size = offset(i64::from_le_bytes(array(&entry[12..20])));
        let data = start
            .zip(size)
            .and_then(|(start, size)| bytes.get(start..start.checked_add(size)?))
            .ok_or_else(|| damaged("a chunk lies outside it"))?;
        match &entry[..4] {
            id if id == COMPONENT_CHUNK => state.component = data.to_vec(),
            id if id == CONTROLLER_CHUNK => state.controller = data.to_vec(),
            _ => {}
        }
    }
    Ok((class, state))
}

/// What a preset file says about itself, short of its states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Glance {
    /// The plugin the preset is for.
    pub class: ClassId,
    /// Its meta information, as the file has it; empty if it has none.
    pub info: Vec<u8>,
}

/// Reads which plugin a preset file is for, and its meta information,
/// without reading the states in it: a few hundred bytes of a file that
/// may hold megabytes. `None` for a file that is not a VST3 preset.
pub(crate) fn glance(file: &mut (impl Read + Seek)) -> Option<Glance> {
    let mut header = [0u8; HEADER_SIZE];
    file.read_exact(&mut header).ok()?;
    if &header[..4] != HEADER_ID || i32::from_le_bytes(array(&header[4..8])) != FORMAT_VERSION {
        return None;
    }
    let class = std::str::from_utf8(&header[8..40]).ok().and_then(ClassId::parse)?;
    // The meta information is optional, and a file whose chunk list is
    // damaged is still one for this class; loading it will say what is
    // wrong with it.
    let info = (|| {
        let list = u64::try_from(i64::from_le_bytes(array(&header[40..48]))).ok()?;
        file.seek(SeekFrom::Start(list)).ok()?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        if &head[..4] != LIST_ID {
            return None;
        }
        let count = usize::try_from(i32::from_le_bytes(array(&head[4..8]))).ok()?;
        for _ in 0..count.min(MAX_CHUNKS) {
            let mut entry = [0u8; ENTRY_SIZE];
            file.read_exact(&mut entry).ok()?;
            if &entry[..4] != INFO_CHUNK {
                continue;
            }
            let start = u64::try_from(i64::from_le_bytes(array(&entry[4..12]))).ok()?;
            let size = usize::try_from(i64::from_le_bytes(array(&entry[12..20]))).ok()?;
            file.seek(SeekFrom::Start(start)).ok()?;
            let mut info = Vec::new();
            file.take(size.min(MAX_INFO) as u64).read_to_end(&mut info).ok()?;
            return Some(info);
        }
        None
    })();
    Some(Glance { class, info: info.unwrap_or_default() })
}

/// `bytes` as an array; the callers slice exactly `N` bytes.
fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut array = [0; N];
    array.copy_from_slice(bytes);
    array
}

/// A stored offset or size as an index, unless it is negative.
fn offset(value: i64) -> Option<usize> {
    usize::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class() -> ClassId {
        ClassId::parse("ABCDEF019182FAEB5776737957767331").unwrap()
    }

    fn both() -> PluginState {
        PluginState { component: b"component bytes".to_vec(), controller: vec![0, 255, 7] }
    }

    #[test]
    fn both_states_round_trip_with_the_class_they_belong_to() {
        let blob = encode(class(), &both());
        assert_eq!(decode(&blob), Ok((class(), both())));

        let only_component = PluginState { component: vec![1, 2, 3], controller: Vec::new() };
        assert_eq!(decode(&encode(class(), &only_component)), Ok((class(), only_component)));
        let only_controller = PluginState { component: Vec::new(), controller: vec![9] };
        assert_eq!(decode(&encode(class(), &only_controller)), Ok((class(), only_controller)));

        let nothing = PluginState::default();
        assert!(nothing.is_empty() && !both().is_empty());
        assert_eq!(decode(&encode(class(), &nothing)), Ok((class(), nothing)));
    }

    #[test]
    fn the_blob_is_a_vst3_preset_file() {
        let state = PluginState { component: b"AB".to_vec(), controller: b"C".to_vec() };
        let blob = encode(class(), &state);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"VST3\x01\0\0\0ABCDEF019182FAEB5776737957767331");
        expected.extend_from_slice(&51i64.to_le_bytes()); // the list follows 48 + 3 bytes
        expected.extend_from_slice(b"ABC");
        expected.extend_from_slice(b"List\x02\0\0\0");
        expected.extend_from_slice(b"Comp");
        expected.extend_from_slice(&48i64.to_le_bytes());
        expected.extend_from_slice(&2i64.to_le_bytes());
        expected.extend_from_slice(b"Cont");
        expected.extend_from_slice(&50i64.to_le_bytes());
        expected.extend_from_slice(&1i64.to_le_bytes());
        assert_eq!(blob, expected);
    }

    #[test]
    fn a_glance_tells_whose_preset_a_file_is_without_reading_its_states() {
        use std::io::Cursor;
        let blob = encode(class(), &both());
        assert_eq!(glance(&mut Cursor::new(&blob)), Some(Glance { class: class(), info: Vec::new() }));

        // A preset written by another host, with its meta information.
        let mut with_info = Vec::new();
        with_info.extend_from_slice(b"VST3\x01\0\0\0ABCDEF019182FAEB5776737957767331");
        with_info.extend_from_slice(&57i64.to_le_bytes());
        with_info.extend_from_slice(b"abc");
        with_info.extend_from_slice(b"<xml/>");
        with_info.extend_from_slice(b"List\x02\0\0\0");
        with_info.extend_from_slice(b"Comp");
        with_info.extend_from_slice(&48i64.to_le_bytes());
        with_info.extend_from_slice(&3i64.to_le_bytes());
        with_info.extend_from_slice(b"Info");
        with_info.extend_from_slice(&51i64.to_le_bytes());
        with_info.extend_from_slice(&6i64.to_le_bytes());
        assert_eq!(glance(&mut Cursor::new(&with_info)).unwrap().info, b"<xml/>");

        // Not a preset file, in any of the ways a file can fail to be.
        let other: [&[u8]; 4] =
            [b"", b"VST3", b"some other file, long enough to hold a whole header in it....", &blob[..47]];
        for bytes in other {
            assert_eq!(glance(&mut Cursor::new(bytes)), None);
        }
        let mut version = blob.clone();
        version[4] = 2;
        assert_eq!(glance(&mut Cursor::new(&version)), None);
        // A chunk list that is cut off or points nowhere costs the meta
        // information, not the class; and no cut is a panic.
        for length in HEADER_SIZE..with_info.len() {
            let cut = glance(&mut Cursor::new(&with_info[..length])).expect("the header is whole");
            assert_eq!(cut.class, class());
        }
        let mut nowhere = with_info.clone();
        nowhere[40..48].copy_from_slice(&i64::MAX.to_le_bytes());
        assert_eq!(glance(&mut Cursor::new(&nowhere)).unwrap().info, b"");
        let mut huge = with_info.clone();
        let size = huge.len() - 8;
        huge[size..].copy_from_slice(&i64::MAX.to_le_bytes());
        assert_eq!(glance(&mut Cursor::new(&huge)).unwrap().info, &huge[51..], "what there is of it");
    }

    #[test]
    fn chunks_this_host_does_not_use_are_skipped() {
        // A preset written by another host, with its meta information first.
        let mut blob = Vec::new();
        blob.extend_from_slice(b"VST3\x01\0\0\0ABCDEF019182FAEB5776737957767331");
        blob.extend_from_slice(&57i64.to_le_bytes());
        blob.extend_from_slice(b"<xml/>");
        blob.extend_from_slice(b"abc");
        blob.extend_from_slice(b"List\x02\0\0\0");
        blob.extend_from_slice(b"Info");
        blob.extend_from_slice(&48i64.to_le_bytes());
        blob.extend_from_slice(&6i64.to_le_bytes());
        blob.extend_from_slice(b"Comp");
        blob.extend_from_slice(&54i64.to_le_bytes());
        blob.extend_from_slice(&3i64.to_le_bytes());
        let (_, state) = decode(&blob).unwrap();
        assert_eq!(state, PluginState { component: b"abc".to_vec(), controller: Vec::new() });
    }

    #[test]
    fn anything_else_is_an_error_that_says_why() {
        let blob = encode(class(), &both());
        let error = |bytes: &[u8]| decode(bytes).unwrap_err();

        assert!(error(b"").contains("too short"));
        assert!(error(b"saved by some other plugin, in some other format entirely").contains("not a VST3 state"));

        let mut version = blob.clone();
        version[4] = 2;
        assert!(error(&version).contains("version (2)"));

        let mut class_id = blob.clone();
        class_id[8] = b'?';
        assert!(error(&class_id).contains("class id"));

        // Every way of cutting the blob short is caught, never a panic.
        for length in 0..blob.len() {
            assert!(decode(&blob[..length]).is_err(), "cut to {length} bytes");
        }
    }

    #[test]
    fn offsets_and_sizes_that_point_outside_the_blob_are_caught() {
        let blob = encode(class(), &both());
        let list = blob.len() - 8 - 2 * ENTRY_SIZE;
        let patched = |at: usize, value: i64| {
            let mut bytes = blob.clone();
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
            decode(&bytes)
        };

        // The list offset in the header.
        for value in [-1, i64::MAX, blob.len() as i64 + 1, 0] {
            assert!(patched(40, value).is_err(), "list offset {value}");
        }
        // The offset and the size of the first chunk.
        let entry = list + 8;
        for value in [-1, i64::MAX, blob.len() as i64] {
            assert!(patched(entry + 4, value).is_err(), "chunk offset {value}");
            assert!(patched(entry + 12, value).is_err(), "chunk size {value}");
        }
        // A count larger than the list, up to the largest there is.
        for count in [3i32, i32::MAX, -1] {
            let mut bytes = blob.clone();
            bytes[list + 4..list + 8].copy_from_slice(&count.to_le_bytes());
            assert!(decode(&bytes).is_err(), "count {count}");
        }
    }
}
