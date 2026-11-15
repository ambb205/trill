//! The index on disk, so that the library is there the moment the program
//! starts instead of after a scan.
//!
//! The file is a cache: everything in it can be rebuilt by scanning. So it
//! is never trusted further than it can be checked, and anything wrong with
//! it (damage, a version this build does not write, a tree that does not
//! hold together) means it is set aside and rebuilt, not repaired.

use crate::entry::{AudioFacts, Entry, EntryKind, without_extension};
use crate::index::{FolderId, Index, IndexBuilder};
use crate::key::Key;
use crate::tags::Tags;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Bump whenever the meaning of the file changes: a field added or
/// reinterpreted, or inference rules changed in a way stored results should
/// not survive. Older files are then rebuilt on first launch.
const VERSION: u32 = 1;

/// The first line of the file. Read on its own, so that the version can be
/// checked before the rest is trusted to have the expected shape.
#[derive(Serialize, Deserialize)]
struct Header {
    #[serde(rename = "trill-library-index")]
    version: u32,
}

#[derive(Serialize, Deserialize)]
struct StoredIndex {
    /// In tree order: every folder after its parent.
    folders: Vec<StoredFolder>,
}

#[derive(Serialize, Deserialize)]
struct StoredFolder {
    /// The whole path for a root; the folder's own name otherwise.
    #[serde(rename = "p")]
    path: String,
    /// Position of the parent folder in the list; absent for a root.
    #[serde(rename = "u", default, skip_serializing_if = "Option::is_none")]
    parent: Option<u32>,
    #[serde(rename = "f", default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<StoredFile>,
}

/// One file. The keys are single letters because there are a hundred
/// thousand of them and the file is read at every launch.
#[derive(Serialize, Deserialize)]
struct StoredFile {
    /// File name, with extension.
    #[serde(rename = "n")]
    name: String,
    #[serde(rename = "k")]
    kind: u8,
    #[serde(rename = "s")]
    size: u64,
    /// Modified time in nanoseconds since 1970.
    #[serde(rename = "m", default, skip_serializing_if = "Option::is_none")]
    modified: Option<i64>,
    /// Sample rate; present for audio whose header was read.
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    sample_rate: Option<u32>,
    #[serde(rename = "c", default, skip_serializing_if = "is_zero")]
    channels: u16,
    /// Length in frames. Not seconds: a whole number is read back exactly
    /// as it was written, which a decimal fraction is not.
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    frames: Option<u64>,
    #[serde(rename = "x", default, skip_serializing_if = "is_false")]
    unreadable: bool,
    #[serde(rename = "b", default, skip_serializing_if = "Option::is_none")]
    tempo: Option<f32>,
    #[serde(rename = "y", default, skip_serializing_if = "Option::is_none")]
    key: Option<u8>,
    #[serde(rename = "t", default, skip_serializing_if = "is_zero_u64")]
    tags: u64,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Why an index file was not used.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LoadError {
    #[error("there is no saved index yet")]
    Missing,
    #[error("the saved index could not be read: {0}")]
    Unreadable(String),
    #[error("the saved index was written by a different version ({0}) and will be rebuilt")]
    Outdated(u32),
    #[error("the saved index is damaged and will be rebuilt: {0}")]
    Damaged(String),
}

/// Reads the index saved at `path`.
pub(crate) fn load(path: &Path) -> Result<Index, LoadError> {
    let bytes = std::fs::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => LoadError::Missing,
        _ => LoadError::Unreadable(error.to_string()),
    })?;
    let damaged = |reason: &dyn std::fmt::Display| LoadError::Damaged(reason.to_string());
    let line_end = bytes.iter().position(|byte| *byte == b'\n').ok_or_else(|| damaged(&"it has no header"))?;
    let header: Header = serde_json::from_slice(&bytes[..line_end]).map_err(|error| damaged(&error))?;
    if header.version != VERSION {
        return Err(LoadError::Outdated(header.version));
    }
    let stored: StoredIndex = serde_json::from_slice(&bytes[line_end + 1..]).map_err(|error| damaged(&error))?;

    let files = stored.folders.iter().map(|folder| folder.files.len()).sum();
    let mut builder = IndexBuilder::with_capacity(files);
    let mut paths: Vec<PathBuf> = Vec::with_capacity(stored.folders.len());
    for folder in stored.folders {
        let (path, parent) = match folder.parent {
            Some(parent) => {
                let parent_path =
                    paths.get(parent as usize).ok_or_else(|| damaged(&"a folder comes before its parent"))?;
                let path = inside(parent_path, &folder.path).ok_or_else(|| damaged(&"a folder name is not a name"))?;
                (path, Some(FolderId(parent)))
            }
            None => (PathBuf::from(&folder.path), None),
        };
        builder.folder(path.clone(), parent).map_err(|_| damaged(&"its folders are out of order"))?;
        for file in folder.files {
            let entry = file.into_entry(&path).ok_or_else(|| damaged(&"it lists a file that cannot be"))?;
            builder.file(entry).map_err(|_| damaged(&"its files are out of order"))?;
        }
        paths.push(path);
    }
    Ok(builder.finish())
}

/// Writes `index` to `path`, replacing what is there only once the new file
/// is complete.
pub(crate) fn save(path: &Path, index: &Index) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec(&Header { version: VERSION })?;
    bytes.push(b'\n');
    serde_json::to_writer(&mut bytes, &stored(index))?;

    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.part", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// The index in its stored form.
///
/// Names that are not valid Unicode cannot be written to a text file and
/// read back as the same name, so those files, and folders with everything
/// in them, are left out. They stay usable in the running program and are
/// simply found again by the next scan.
fn stored(index: &Index) -> StoredIndex {
    let mut folders: Vec<StoredFolder> = Vec::with_capacity(index.folders.len());
    // Where each folder of the index went in `folders`, if it was written.
    let mut written: Vec<Option<u32>> = Vec::with_capacity(index.folders.len());
    for folder in &index.folders {
        let (path, parent) = match folder.parent() {
            None => (folder.path().to_str(), Some(None)),
            // A folder whose parent was left out is left out with it.
            Some(parent) => {
                (folder.path().file_name().and_then(|name| name.to_str()), written[parent.0 as usize].map(Some))
            }
        };
        let (Some(path), Some(parent)) = (path, parent) else {
            written.push(None);
            continue;
        };
        let files = index.entries()[folder.files()].iter().filter_map(StoredFile::of).collect();
        written.push(Some(folders.len() as u32));
        folders.push(StoredFolder { path: path.to_string(), parent, files });
    }
    StoredIndex { folders }
}

impl StoredFile {
    fn of(entry: &Entry) -> Option<Self> {
        Some(Self {
            name: entry.path.file_name()?.to_str()?.to_string(),
            kind: entry.kind.code(),
            size: entry.size,
            modified: entry.modified.and_then(to_nanoseconds),
            sample_rate: entry.audio.map(|audio| audio.sample_rate),
            channels: entry.audio.map_or(0, |audio| audio.channels),
            frames: entry.audio.and_then(|audio| audio.frames),
            unreadable: entry.unreadable,
            tempo: entry.tempo,
            key: entry.key.map(Key::code),
            tags: entry.tags.bits(),
        })
    }

    /// `None` if the kind is not one this version knows or the name is not
    /// a file name.
    fn into_entry(self, folder: &Path) -> Option<Entry> {
        let kind = EntryKind::from_code(self.kind)?;
        let path = inside(folder, &self.name)?;
        let audio = self
            .sample_rate
            .filter(|rate| kind == EntryKind::Audio && *rate > 0)
            .map(|sample_rate| AudioFacts { frames: self.frames, sample_rate, channels: self.channels });
        Some(Entry {
            name: without_extension(&self.name).to_string(),
            path,
            kind,
            size: self.size,
            modified: self.modified.and_then(from_nanoseconds),
            audio,
            unreadable: self.unreadable && kind == EntryKind::Audio && audio.is_none(),
            tempo: self.tempo.filter(|tempo| tempo.is_finite()),
            key: self.key.and_then(Key::from_code),
            tags: Tags::from_bits(self.tags),
        })
    }
}

/// The path of `name` inside `folder`, or `None` if `name` is not a plain
/// name. Names in the file are only ever joined to the folder they were
/// found in; one that could lead elsewhere (`..`, or a path) means the file
/// was not written by this program.
///
/// Built by hand because this runs once per file at every launch, and
/// `Path::join` works out a good deal that is already known here.
fn inside(folder: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains(std::path::is_separator) {
        return None;
    }
    let folder = folder.as_os_str();
    let mut path = OsString::with_capacity(folder.len() + 1 + name.len());
    path.push(folder);
    let ends_with_separator =
        folder.as_encoded_bytes().last().is_some_and(|last| std::path::is_separator(char::from(*last)));
    if !folder.is_empty() && !ends_with_separator {
        path.push(std::path::MAIN_SEPARATOR_STR);
    }
    path.push(name);
    Some(PathBuf::from(path))
}

fn to_nanoseconds(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).ok(),
        Err(before) => i64::try_from(before.duration().as_nanos()).ok().map(|nanoseconds: i64| -nanoseconds),
    }
}

fn from_nanoseconds(nanoseconds: i64) -> Option<SystemTime> {
    let distance = Duration::from_nanos(nanoseconds.unsigned_abs());
    if nanoseconds >= 0 { UNIX_EPOCH.checked_add(distance) } else { UNIX_EPOCH.checked_sub(distance) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::{entry, index_of};
    use crate::tags::Tag;

    fn sample() -> Index {
        let mut kick = entry("Root/Drums/Kick 1.wav");
        kick.size = 123_456;
        kick.modified = Some(UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_700));
        kick.audio = Some(AudioFacts { frames: Some(19_271), sample_rate: 44_100, channels: 1 });
        kick.tags = Tags::of(&[Tag::Kick, Tag::Drum, Tag::OneShot]);
        let mut looped = entry("Root/Loops/Melody 140 F#m.flac");
        looped.audio = Some(AudioFacts { frames: None, sample_rate: 96_000, channels: 2 });
        looped.tempo = Some(140.0);
        looped.key = Key::parse("F#m");
        looped.modified = Some(UNIX_EPOCH - Duration::from_secs(86_400));
        let mut broken = entry("Root/Loops/broken.aif");
        broken.unreadable = true;
        let pending = entry("Root/Loops/new.ogg");
        index_of(vec![entry("Root/song.trill"), kick, looped, broken, pending, entry("Second Root/riff.mid")])
    }

    #[test]
    fn an_index_survives_the_round_trip() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("cache/library.json");
        let index = sample();
        save(&path, &index).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.entries(), index.entries());
        assert_eq!(loaded.folders, index.folders);
        assert_eq!(loaded.roots(), index.roots());
        assert!(loaded.entries()[4].is_pending(), "a file not yet examined stays so");
        assert_eq!(loaded.find(Path::new("Second Root/riff.mid")), index.find(Path::new("Second Root/riff.mid")));

        // Saving again replaces the file and leaves nothing else behind.
        save(&path, &Index::default()).unwrap();
        assert!(load(&path).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn anything_wrong_with_the_file_is_an_error_not_a_crash() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("library.json");
        assert_eq!(load(&path).err(), Some(LoadError::Missing));

        save(&path, &sample()).unwrap();
        let good = std::fs::read(&path).unwrap();
        let write = |bytes: &[u8]| std::fs::write(&path, bytes).unwrap();

        write(b"");
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))));
        write(b"not json at all\n{}");
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))));
        write(&good[..good.len() / 2]);
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))), "a file cut short");
        write(b"{\"trill-library-index\":999}\n{\"folders\":[]}");
        assert_eq!(load(&path).err(), Some(LoadError::Outdated(999)));
        write(b"{\"trill-library-index\":1}\n{\"folders\":\"none\"}");
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))));
        // A tree that does not hold together.
        write(b"{\"trill-library-index\":1}\n{\"folders\":[{\"p\":\"a\",\"u\":0}]}");
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))), "a folder that is its own parent");
        write(b"{\"trill-library-index\":1}\n{\"folders\":[{\"p\":\"a\"},{\"p\":\"b\",\"u\":7}]}");
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))));
        write(b"{\"trill-library-index\":1}\n{\"folders\":[{\"p\":\"a\",\"f\":[{\"n\":\"x.wav\",\"k\":77,\"s\":1}]}]}");
        assert!(matches!(load(&path), Err(LoadError::Damaged(_))), "a kind nobody knows");
        // Names that would lead out of the folder they are listed in.
        for name in ["../../elsewhere.wav", "..", "", "sub/x.wav"] {
            let head = "{\"trill-library-index\":1}\n";
            write(
                format!("{head}{{\"folders\":[{{\"p\":\"a\",\"f\":[{{\"n\":\"{name}\",\"k\":0,\"s\":1}}]}}]}}")
                    .as_bytes(),
            );
            assert!(matches!(load(&path), Err(LoadError::Damaged(_))), "the file name '{name}'");
            write(format!("{head}{{\"folders\":[{{\"p\":\"a\"}},{{\"p\":\"{name}\",\"u\":0}}]}}").as_bytes());
            assert!(matches!(load(&path), Err(LoadError::Damaged(_))), "the folder name '{name}'");
        }

        // Every single flipped byte either loads or is refused.
        let mut dice = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..300 {
            dice ^= dice << 13;
            dice ^= dice >> 7;
            dice ^= dice << 17;
            let mut bytes = good.clone();
            let at = (dice % bytes.len() as u64) as usize;
            bytes[at] = (dice >> 32) as u8;
            write(&bytes);
            let _ = load(&path);
        }

        // Odd values in a well-formed file are tidied, not trusted.
        write(
            b"{\"trill-library-index\":1}\n{\"folders\":[{\"p\":\"a\",\"f\":[{\"n\":\"x.mid\",\"k\":1,\"s\":1,\"r\":44100,\"x\":true,\"y\":99,\"t\":18446744073709551615,\"m\":-9223372036854775808}]}]}",
        );
        let odd = load(&path).unwrap();
        let entry = &odd.entries()[0];
        assert_eq!((entry.audio, entry.key), (None, None), "a MIDI file has no audio facts");
        assert!(!entry.unreadable && entry.tags.len() < 64);
    }

    #[test]
    fn paths_are_put_together_the_way_the_scanner_does() {
        let folders = if cfg!(windows) {
            vec!["a", "a\\b c", "C:\\", "C:\\Users\\someone", "trailing\\", "forward/slashes"]
        } else {
            vec!["a", "a/b c", "/", "/abs/olute", "trailing/"]
        };
        for folder in folders {
            for name in ["Kick 01.wav", "ünïcode.flac", "no extension", ".hidden", "a.b.c"] {
                assert_eq!(inside(Path::new(folder), name), Some(Path::new(folder).join(name)), "{folder} + {name}");
            }
        }
        assert_eq!(inside(Path::new(""), "x.wav"), Some(PathBuf::from("x.wav")));
        assert_eq!(inside(Path::new("a"), "b/c.wav"), None);
    }

    #[test]
    fn times_round_trip_to_the_nanosecond() {
        for time in [
            UNIX_EPOCH,
            UNIX_EPOCH + Duration::new(1_731_700_000, 999_999_999),
            UNIX_EPOCH - Duration::new(5, 1),
            UNIX_EPOCH + Duration::from_secs(4_000_000_000),
        ] {
            assert_eq!(to_nanoseconds(time).and_then(from_nanoseconds), Some(time));
        }
        // The year 2286: representable as a time, not as 64 bits of nanoseconds.
        assert_eq!(to_nanoseconds(UNIX_EPOCH + Duration::from_secs(10_000_000_000)), None, "too far off to store");
    }
}
