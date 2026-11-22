//! The files a song plays, and what is done when they are not where the
//! song says.
//!
//! A song names audio files by their paths: every audio clip one, and every
//! sample a device plays. Folders get moved, and then the song opens with
//! clips that are silent. This module holds what the rest of the program
//! needs to put that right, without touching a disk:
//!
//! * [`Project::files`] lists every place the song names a file
//!   ([`FileUse`]), and [`Project::relink`] gives places other paths: one
//!   edit, however many places.
//! * A device's saved state is its own business, so which files it names is
//!   asked of whoever made the device, through [`DeviceFiles`].
//! * [`FolderMove`] is the arithmetic of a folder that went elsewhere: read
//!   from where one file was and where it is now, and applied to the others
//!   that were under it.
//! * [`FileTable`] is what a project file says about its files beside
//!   their paths: where each is from the song's own folder, and what kind of
//!   file it was. See below.
//!
//! # A song and its files, in the project file
//!
//! A path is absolute: it means one place on one machine. So that a folder
//! holding a song and its samples can be moved or sent whole, the project
//! file also says where each file is *from the song's own folder*
//! (`beside`, with `/` between the parts), and what the file was when the
//! song was saved (its size, length, sample rate and channels):
//!
//! ```json
//! "files": [
//!   { "path": "D:/Songs/Night/samples/kick.wav", "beside": "samples/kick.wav",
//!     "bytes": 88244, "frames": 22050, "sample_rate": 44100, "channels": 2 }
//! ]
//! ```
//!
//! **The file beside the song wins.** When a song is opened, a file that is
//! found where `beside` leads from the folder the song is in now, and that
//! is the file the song knew (the same size and length), is the one the
//! song plays; the path is brought in line. Otherwise the path is used as
//! it stands. If neither leads to a file, the file is missing, and what was
//! written down about it is what tells the right file from one that merely
//! has its name.
//!
//! The table is beside the song in the file, not part of the song: an edit
//! never touches it, a build that does not know it reads the song without
//! it, and a file written before there was one simply has none.

use crate::ids::{ClipId, DeviceId, TrackId};
use crate::project::{Clip, ClipContent, Device, EditError, Project};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---- what a file is ---------------------------------------------------------
//
// A path means one place on one machine, so a song that several programs
// hold cannot know a file by it. There a file is known by what it *is*:
// its bytes, by their SHA-256 and their number. Where it lies is each
// machine's own, said to nobody and taken from nobody
// (`merge::Key::is_personal`).

/// The bytes of a file, whoever has them and wherever: their SHA-256 and
/// how many they are. This is what one program asks another for a file
/// by. It is never a place.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Content {
    /// The SHA-256 of the bytes: 64 digits, `0-9a-f`.
    pub sha256: String,
    pub bytes: u64,
}

/// The kinds of file a song plays, by the ending of their names: what a
/// file that came from another program is named with here, so that
/// whatever reads it knows what it is reading. A name another program
/// said chooses one of these and is never part of a name on this disk.
const KINDS: [&str; 17] = [
    "wav", "wave", "aif", "aiff", "aifc", "flac", "mp3", "ogg", "oga", "opus", "m4a", "mp4", "aac", "caf", "w64", "wv",
    "sfz",
];

/// The kind of file a name says it is: one of a fixed list, or `bin` for
/// a name that says none of them. Whatever the name is, what comes back
/// is text of this program's own.
pub fn kind_of(name: &str) -> &'static str {
    let ending = name.rsplit_once('.').map(|(_, ending)| ending.to_ascii_lowercase()).unwrap_or_default();
    KINDS.iter().copied().find(|kind| *kind == ending).unwrap_or("bin")
}

impl Content {
    /// The content of a file whose bytes hash to `sha256`.
    pub fn new(sha256: &[u8; 32], bytes: u64) -> Self {
        let sha256 = sha256.iter().map(|byte| format!("{byte:02x}")).collect();
        Self { sha256, bytes }
    }

    /// Whether this is written as one is: a hash of 64 digits in lower
    /// case. Says nothing about whether any file has these bytes.
    pub fn is_well_formed(&self) -> bool {
        self.sha256.len() == 64 && self.sha256.bytes().all(|digit| matches!(digit, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// The hash as bytes; `None` for one that is not well formed.
    pub fn hash(&self) -> Option<[u8; 32]> {
        if !self.is_well_formed() {
            return None;
        }
        let mut hash = [0u8; 32];
        for (at, byte) in hash.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&self.sha256[at * 2..at * 2 + 2], 16).ok()?;
        }
        Some(hash)
    }
}

impl std::fmt::Display for Content {
    /// A few digits of it and its size: enough to tell two apart in a
    /// sentence or a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} bytes)", self.sha256.get(..8).unwrap_or("?"), self.bytes)
    }
}

/// What a song says a file it plays is: its content, and the name its
/// maker has it under. The name is for showing and for the kind of file it
/// is; it is the last part of a path and never a path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileIs {
    pub sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl FileIs {
    /// What the file at `path` is, given the hash of its bytes: named by
    /// the last part of the path.
    pub fn new(content: Content, path: &str) -> Self {
        let name: String = file_name(path).chars().filter(|c| !c.is_control()).take(MAX_FILE_NAME).collect();
        Self { sha256: content.sha256, bytes: content.bytes, name }
    }

    /// The bytes it is, without the name.
    pub fn content(&self) -> Content {
        Content { sha256: self.sha256.clone(), bytes: self.bytes }
    }

    /// Whether this is written as one is: a well-formed hash, and a name
    /// that is a name (no part of a path, nothing that is not text, of a
    /// length).
    pub fn is_well_formed(&self) -> bool {
        let named = self.name.chars().count() <= MAX_FILE_NAME
            && !self.name.chars().any(|c| c.is_control() || is_separator(c) || c == ':');
        named && self.content().is_well_formed()
    }

    /// The kind of file it is, by its name: one of a fixed list, or
    /// `bin` for a name that says none of them.
    pub fn kind(&self) -> &'static str {
        kind_of(&self.name)
    }

    /// The name the file has in a folder of files that came from other
    /// programs: its hash and its kind. Made here, of nothing a program
    /// could choose but which of the kinds it is; two files of the same
    /// bytes are one file there.
    pub fn kept_as(&self) -> String {
        format!("{}.{}", self.sha256, self.kind())
    }
}

/// The longest name a file is said by.
pub const MAX_FILE_NAME: usize = 255;

/// Where a song names a file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FilePlace {
    /// The audio an audio clip plays.
    Clip(ClipId),
    /// A sample of a device: `slot` is what the device calls it, such as
    /// `sample` on a sampler and `pad 36` on a drum rack.
    Sample { device: DeviceId, slot: String },
}

impl std::fmt::Display for FilePlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilePlace::Clip(clip) => clip.fmt(f),
            FilePlace::Sample { device, slot } => write!(f, "{slot} of {device}"),
        }
    }
}

/// One place a song names a file, and what is there.
#[derive(Clone, Debug, PartialEq)]
pub struct FileUse {
    pub place: FilePlace,
    /// The path as the song has it.
    pub path: String,
    /// The track the clip is on or the device is in; `None` for a device
    /// of the master.
    pub track: Option<TrackId>,
    /// The clip's name, or the device's.
    pub name: String,
    /// How far into the file the clip plays, in seconds: its offset and
    /// then its length. A file shorter than this is not all the clip wants.
    /// `None` for a device's sample.
    pub reach_seconds: Option<f64>,
}

/// A place a song names a file, and the path it is to name instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relink {
    pub place: FilePlace,
    pub path: String,
}

/// What knows which files a device's saved state names: whoever made the
/// device. The song itself only keeps the state.
pub trait DeviceFiles {
    /// The files `device` plays, each with the slot it is in. Empty for a
    /// device that plays none, or whose state cannot be read.
    fn files(&self, device: &Device) -> Vec<(String, String)>;

    /// The state of `device` with the file in each of `slots` (slot, path)
    /// replaced by that path, and everything else as it is. An error says
    /// in words why that cannot be done.
    fn relinked(&self, device: &Device, slots: &[(String, String)]) -> Result<Vec<u8>, String>;
}

/// For a song whose devices play no files, or when only the clips matter.
pub struct NoDeviceFiles;

impl DeviceFiles for NoDeviceFiles {
    fn files(&self, _device: &Device) -> Vec<(String, String)> {
        Vec::new()
    }

    fn relinked(&self, device: &Device, _slots: &[(String, String)]) -> Result<Vec<u8>, String> {
        Err(format!("{} ({}) plays no files", device.id, device.name))
    }
}

impl Project {
    /// Every place the song names a file, in the order of the tracks: each
    /// track's clips and then its devices' samples, and the master's last.
    pub fn files(&self, devices: &dyn DeviceFiles) -> Vec<FileUse> {
        let mut uses = Vec::new();
        let of_devices = |chain: &[Device], track: Option<TrackId>, uses: &mut Vec<FileUse>| {
            for device in chain {
                for (slot, path) in devices.files(device) {
                    uses.push(FileUse {
                        place: FilePlace::Sample { device: device.id, slot },
                        path,
                        track,
                        name: device.name.clone(),
                        reach_seconds: None,
                    });
                }
            }
        };
        for track in &self.tracks {
            for clip in &track.clips {
                let Some(audio) = clip.audio() else { continue };
                uses.push(FileUse {
                    place: FilePlace::Clip(clip.id),
                    path: audio.path.clone(),
                    track: Some(track.id),
                    name: clip.name.clone(),
                    // A clip that plays round reaches the end of its loop and no further.
                    reach_seconds: Some(match audio.round() {
                        Some(round) => round.end_seconds(),
                        None => audio.offset_seconds + clip.length * audio.seconds_per_beat(self.tempo),
                    }),
                });
            }
            // A take's recording is the song's too, played or not: one
            // that is gone is missed, and looked for, like any other.
            for take in &track.takes {
                let Some(audio) = take.clip.audio() else { continue };
                uses.push(FileUse {
                    place: FilePlace::Clip(take.clip.id),
                    path: audio.path.clone(),
                    track: Some(track.id),
                    name: take.clip.name.clone(),
                    reach_seconds: Some(audio.offset_seconds + take.clip.length * audio.seconds_per_beat(self.tempo)),
                });
            }
            of_devices(&track.devices, Some(track.id), &mut uses);
        }
        of_devices(&self.master.devices, None, &mut uses);
        uses
    }

    /// Gives places of the song other files to play: the clips their
    /// paths, the devices their states with the new paths in them. Nothing
    /// else about a clip or a device changes, so a clip goes on playing the
    /// stretch of its file it played, and a pad keeps its settings.
    ///
    /// All of them or none: a place the song does not have, a clip that
    /// plays no file, a slot the device has nothing in or an empty path is
    /// an error that names it, and then nothing is changed. Returns how
    /// many places now name another file than before; a place given the
    /// path it has is not counted.
    pub fn relink(&mut self, relinks: &[Relink], devices: &dyn DeviceFiles) -> Result<usize, EditError> {
        let invalid = |message: String| Err(EditError::Invalid(message));
        let mut changed = 0;
        // The slots of each device, gathered so that a device's state is
        // written once. A place named twice takes the path given last.
        let mut clips: BTreeMap<ClipId, &str> = BTreeMap::new();
        let mut slots: BTreeMap<DeviceId, BTreeMap<&str, &str>> = BTreeMap::new();
        for Relink { place, path } in relinks {
            if path.trim().is_empty() {
                return invalid(format!("{place} was given no file to play: the path is empty"));
            }
            match place {
                FilePlace::Clip(clip) => {
                    clips.insert(*clip, path);
                }
                FilePlace::Sample { device, slot } => {
                    slots.entry(*device).or_default().insert(slot, path);
                }
            }
        }

        // Everything is looked at before anything is changed.
        for (id, path) in &clips {
            let clip = self.recording(*id)?;
            let Some(audio) = clip.audio() else {
                return invalid(format!("{id} ({}) is a MIDI clip: it plays no file", clip.name));
            };
            changed += usize::from(audio.path != *path);
        }
        let mut states: Vec<(DeviceId, Vec<u8>)> = Vec::new();
        for (id, wanted) in &slots {
            let device = self.device(*id)?;
            let has = devices.files(device);
            let mut differing: Vec<(String, String)> = Vec::new();
            for (slot, path) in wanted {
                match has.iter().find(|(known, _)| known == slot) {
                    Some((_, current)) if current == path => {}
                    Some(_) => differing.push(((*slot).to_owned(), (*path).to_owned())),
                    None if has.is_empty() => {
                        return invalid(format!("{id} ({}) plays no files, so it has none in '{slot}'", device.name));
                    }
                    None => {
                        let known: Vec<&str> = has.iter().map(|(slot, _)| slot.as_str()).collect();
                        return invalid(format!(
                            "{id} ({}) has no file in '{slot}'; its files are in: {}",
                            device.name,
                            known.join(", ")
                        ));
                    }
                }
            }
            if differing.is_empty() {
                continue;
            }
            changed += differing.len();
            let state = devices.relinked(device, &differing).map_err(EditError::Invalid)?;
            states.push((*id, state));
        }

        for (id, path) in clips {
            if let ClipContent::Audio(audio) = &mut self.recording_mut(id)?.content {
                audio.path = path.to_owned();
            }
        }
        for (id, state) in states {
            self.device_mut(id)?.state = Some(state);
        }
        Ok(changed)
    }
}

impl Project {
    /// A clip of a track, or a take kept beside one: whichever has `id`.
    /// Both name a file, and both are found by the place that names it.
    fn recording(&self, id: ClipId) -> Result<&Clip, EditError> {
        let takes = self.tracks.iter().flat_map(|track| track.takes.iter().map(|take| &take.clip));
        self.clip(id).or_else(|missing| takes.clone().find(|clip| clip.id == id).ok_or(missing))
    }

    fn recording_mut(&mut self, id: ClipId) -> Result<&mut Clip, EditError> {
        if self.clip(id).is_ok() {
            return self.clip_mut(id);
        }
        let takes = self.tracks.iter_mut().flat_map(|track| track.takes.iter_mut().map(|take| &mut take.clip));
        takes.into_iter().find(|clip| clip.id == id).ok_or(EditError::ClipNotFound(id))
    }
}

// ---- paths -------------------------------------------------------------------
//
// A song may have been made on another system than the one that opens it,
// so its paths are read as text here: either slash parts them.

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Whether two names of a file or folder are the same name. Where the
/// file system does not tell `Kick.wav` from `kick.wav`, neither does this.
fn same_name(a: &str, b: &str) -> bool {
    same_name_folding(a, b, cfg!(any(windows, target_os = "macos")))
}

fn same_name_folding(a: &str, b: &str, fold: bool) -> bool {
    if fold { a.to_lowercase() == b.to_lowercase() } else { a == b }
}

/// Whether two paths are the same path, however their slashes lean.
pub fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (parts(a.trim_end_matches(is_separator)), parts(b.trim_end_matches(is_separator)));
    a.len() == b.len() && a.iter().zip(&b).all(|(a, b)| same_name(a, b))
}

/// How many parts two paths end alike in, the file's name counted: 3 for
/// `A/kit/kicks/k.wav` and `B/kit/kicks/k.wav`, 0 for files of other names.
pub fn alike(a: &str, b: &str) -> usize {
    let (a, b) = (parts(a), parts(b));
    a.iter().rev().zip(b.iter().rev()).take_while(|(a, b)| same_name(a, b)).count()
}

/// The parts of a path, as written. A path that starts with a slash has an
/// empty first part.
fn parts(path: &str) -> Vec<&str> {
    path.split(is_separator).collect()
}

/// The slash a path is written with: its own if it has one, the system's
/// otherwise.
fn separator_of(path: &str) -> char {
    path.chars().find(|c| is_separator(*c)).unwrap_or(std::path::MAIN_SEPARATOR)
}

/// The last part of a path, which is what a file is known by.
pub fn file_name(path: &str) -> &str {
    path.rsplit(is_separator).find(|part| !part.is_empty()).unwrap_or(path)
}

/// The folder a file is in: its path without the last part. Empty for a
/// bare name.
pub fn folder_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches(is_separator);
    match trimmed.rfind(is_separator) {
        Some(at) => &trimmed[..at],
        None => "",
    }
}

/// `path` without its last `count` parts, and those parts.
fn split_tail(path: &str, count: usize) -> Option<(&str, &str)> {
    let mut at = path.len();
    for _ in 0..count {
        at = path[..at].rfind(is_separator)?;
    }
    Some((&path[..at], path.get(at + 1..)?))
}

/// What is left of `path` below `folder`, if it lies there: `x/y.wav` for
/// `A/x/y.wav` below `A`. `None` for a path elsewhere, and for the folder
/// itself.
pub fn below<'a>(folder: &str, path: &'a str) -> Option<&'a str> {
    let folder = folder.trim_end_matches(is_separator);
    let head = path.get(..folder.len())?;
    let rest = path.get(folder.len()..)?;
    let parted = rest.starts_with(is_separator);
    let same =
        parts(head).len() == parts(folder).len() && parts(head).iter().zip(parts(folder)).all(|(a, b)| same_name(a, b));
    (parted && same).then(|| rest.trim_start_matches(is_separator)).filter(|rest| !rest.is_empty())
}

/// A folder that is somewhere else than it was.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderMove {
    pub from: String,
    pub to: String,
}

impl FolderMove {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self { from: from.into(), to: to.into() }
    }

    /// The move that would explain a file that was at `was` being at `is`,
    /// and how many parts the two paths end alike in, the file's name
    /// counted. `None` when they do not even share the name, or are the
    /// same path.
    ///
    /// The folder that moved is taken to be the topmost one that kept its
    /// name: for `A/packs/drums/kick.wav` found at `B/new/packs/drums/kick.wav`
    /// that is `packs`, which went from `A/packs` to `B/new/packs`. When
    /// only the file's name is the same, it is the folder the file was in.
    pub fn between(was: &str, is: &str) -> Option<(FolderMove, usize)> {
        let (old, new) = (parts(was), parts(is));
        let alike = alike(was, is);
        if alike == 0 || (alike == old.len() && alike == new.len()) {
            return None;
        }
        // The parts below the folder that moved.
        let below = if alike >= 2 { alike - 1 } else { 1 };
        let (from, _) = split_tail(was, below)?;
        let (to, _) = split_tail(is, below)?;
        (!from.is_empty() && !to.is_empty()).then(|| (FolderMove::new(from, to), alike))
    }

    /// Where `path` is if it went along with the folder: `None` for a path
    /// that was not in it.
    pub fn apply(&self, path: &str) -> Option<String> {
        let rest = below(&self.from, path)?;
        let separator = separator_of(&self.to);
        let rest: String = rest.chars().map(|c| if is_separator(c) { separator } else { c }).collect();
        Some(format!("{}{separator}{rest}", self.to.trim_end_matches(is_separator)))
    }

    /// The move of the folder one up, if both folders kept their own name
    /// or not: `A/packs -> B/packs2` for `A/packs/drums -> B/packs2/drums`.
    /// What a folder that was moved *and* renamed looks like from two of
    /// the folders in it.
    pub fn parent(&self) -> Option<FolderMove> {
        let (from, to) = (folder_of(&self.from), folder_of(&self.to));
        let kept = same_name(file_name(&self.from), file_name(&self.to));
        (kept && !from.is_empty() && !to.is_empty() && !same_name(from, to)).then(|| FolderMove::new(from, to))
    }
}

/// Where `path` is from `folder`, with `/` between the parts and `..` for
/// each step up: `samples/kick.wav`, `../packs/kick.wav`. `None` when there
/// is no way from one to the other (another drive), or either is not a
/// whole path.
pub fn relative_to(folder: &str, path: &str) -> Option<String> {
    let folder = folder.trim_end_matches(is_separator);
    let (from, to) = (parts(folder), parts(path));
    // The first part is the drive, or empty for a path from the root:
    // without it in common there is no way across.
    let rooted = |parts: &[&str]| parts.len() >= 2 && (parts[0].is_empty() || parts[0].ends_with(':'));
    if !rooted(&from) || !rooted(&to) || !same_name(from[0], to[0]) {
        return None;
    }
    let shared = from.iter().zip(&to).take_while(|(a, b)| same_name(a, b)).count();
    // A share on another machine is named by the two parts after `\\`.
    if from[0].is_empty() && from.get(1).is_some_and(|part| part.is_empty()) && shared < 4 {
        return None;
    }
    let up = from.len() - shared;
    let down = &to[shared..];
    if down.is_empty() || down.iter().any(|part| part.is_empty()) {
        return None;
    }
    let steps = std::iter::repeat_n("..", up).chain(down.iter().copied());
    Some(steps.collect::<Vec<_>>().join("/"))
}

/// The path that `relative` (as [`relative_to`] writes it) leads to from
/// `folder`, written the way `folder` is. `None` when it leads above the
/// top of `folder`'s drive.
pub fn resolve(folder: &str, relative: &str) -> Option<String> {
    let separator = separator_of(folder);
    let mut out: Vec<&str> = parts(folder.trim_end_matches(is_separator));
    for part in relative.split(is_separator).filter(|part| !part.is_empty() && *part != ".") {
        if part == ".." {
            // The drive, or the root, is not a folder to step out of.
            if out.len() <= 1 {
                return None;
            }
            out.pop();
        } else {
            out.push(part);
        }
    }
    Some(out.join(&separator.to_string()))
}

// ---- what a project file says about its files -------------------------------

/// What a project file says about one of the files its song plays, beside
/// the path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownFile {
    /// The path as the song has it.
    pub path: String,
    /// Where the file is from the folder the song was saved in; see
    /// [`relative_to`]. Left out when there is no way from one to the
    /// other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beside: Option<String>,
    /// The size of the file in bytes, when the song was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Its length in frames, when its header says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
}

impl KnownFile {
    /// Whether anything was written down about what the file was.
    pub fn says_what_it_was(&self) -> bool {
        self.bytes.is_some() || self.frames.is_some() || self.sample_rate.is_some()
    }

    /// Whether a file that is `other` is the file this tells of, as far as
    /// both say: `None` when there is nothing to go by. A length or a size
    /// that differs is another file; so is another sample rate.
    pub fn is(&self, other: &KnownFile) -> Option<bool> {
        let pairs = [
            (self.bytes, other.bytes),
            (self.frames, other.frames),
            (self.sample_rate.map(u64::from), other.sample_rate.map(u64::from)),
            (self.channels.map(u64::from), other.channels.map(u64::from)),
        ];
        let mut compared = false;
        for (a, b) in pairs {
            if let (Some(a), Some(b)) = (a, b) {
                if a != b {
                    return Some(false);
                }
                compared = true;
            }
        }
        compared.then_some(true)
    }

    /// The length in seconds, when both the frames and the rate are known.
    pub fn seconds(&self) -> Option<f64> {
        let (frames, rate) = (self.frames?, self.sample_rate?);
        (rate > 0).then(|| frames as f64 / f64::from(rate))
    }
}

/// What a project file says about the files its song plays: one entry per
/// file, in the order of their paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileTable {
    files: Vec<KnownFile>,
}

impl FileTable {
    /// A table of `files`. The last entry for a path is the one kept.
    pub fn new(files: impl IntoIterator<Item = KnownFile>) -> Self {
        let by_path: BTreeMap<String, KnownFile> = files.into_iter().map(|file| (file.path.clone(), file)).collect();
        Self { files: by_path.into_values().collect() }
    }

    pub fn files(&self) -> &[KnownFile] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// What was written down about the file the song has at `path`.
    pub fn get(&self, path: &str) -> Option<&KnownFile> {
        self.files.binary_search_by(|file| file.path.as_str().cmp(path)).ok().map(|at| &self.files[at])
    }

    /// What is known of the file at `was` is known of `is` too: the file
    /// was found there. The entry for `was` stays, for whatever still
    /// names it and for a step taken back.
    pub fn found_at(&mut self, was: &str, is: &str) {
        let Some(known) = self.get(was).cloned() else { return };
        let mut files = std::mem::take(&mut self.files);
        files.retain(|file| file.path != is);
        files.push(KnownFile { path: is.to_owned(), beside: None, ..known });
        *self = Self::new(files);
    }
}

/// The text of a project file: `project`, and what is known of its files.
/// A project file without a table is exactly [`Project::to_json`].
pub fn write(project: &Project, files: &FileTable) -> String {
    write_shared(project, files, None)
}

/// [`write`], for a song that other programs hold too: with what is held
/// beside the song ([`Kept`](crate::merge::Kept)), which is what lets it
/// be closed, worked on alone and merged with the others' again. Like the
/// table of files it is beside the song and no part of it: a program that
/// knows nothing of it reads the song.
pub fn write_shared(project: &Project, files: &FileTable, shared: Option<&crate::merge::Kept>) -> String {
    fn none(files: &&[KnownFile]) -> bool {
        files.is_empty()
    }
    #[derive(Serialize)]
    struct WithFiles<'a> {
        #[serde(flatten)]
        project: &'a Project,
        #[serde(skip_serializing_if = "none")]
        files: &'a [KnownFile],
        #[serde(skip_serializing_if = "Option::is_none")]
        shared: Option<&'a crate::merge::Kept>,
    }
    serde_json::to_string_pretty(&WithFiles { project, files: files.files(), shared })
        .expect("project model is always serializable")
}

/// What a project file of a shared song holds beside the song, if it is
/// one. A file whose section makes no sense is refused in words: opened
/// as a song of one's own it would no longer merge with the others'.
pub fn read_shared(json: &str) -> Result<Option<crate::merge::Kept>, EditError> {
    #[derive(Deserialize)]
    struct Shared {
        #[serde(default)]
        shared: Option<crate::merge::Kept>,
    }
    match serde_json::from_str::<Shared>(json) {
        Ok(read) => Ok(read.shared),
        Err(error) => Err(EditError::Invalid(format!(
            "what this file keeps of the song it shares with others cannot be read: {error}"
        ))),
    }
}

/// Reads a project file: the song as [`Project::from_json`] reads it, and
/// its table of files. A file without a table, or with one that makes no
/// sense, is a song with an empty table: the table is a help, and never a
/// reason not to open a song.
pub fn read(json: &str) -> Result<(Project, FileTable), EditError> {
    #[derive(Deserialize)]
    struct Files {
        #[serde(default)]
        files: Vec<KnownFile>,
    }
    let project = Project::from_json(json)?;
    let files = serde_json::from_str::<Files>(json).map(|read| read.files).unwrap_or_default();
    Ok((project, FileTable::new(files.into_iter().filter(|file| !file.path.is_empty()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{AudioContent, ChainRef, WarpMode};

    /// Devices whose state is their files, a line each: `slot=path`.
    struct Lines;

    fn lines(device: &Device) -> Vec<(String, String)> {
        let text = String::from_utf8_lossy(device.state.as_deref().unwrap_or_default()).into_owned();
        text.lines().filter_map(|line| line.split_once('=')).map(|(slot, path)| (slot.into(), path.into())).collect()
    }

    impl DeviceFiles for Lines {
        fn files(&self, device: &Device) -> Vec<(String, String)> {
            lines(device)
        }

        fn relinked(&self, device: &Device, slots: &[(String, String)]) -> Result<Vec<u8>, String> {
            if slots.iter().any(|(_, path)| path.ends_with(".txt")) {
                return Err("a text file is not a sample".into());
            }
            let mut files = lines(device);
            for (slot, path) in slots {
                files.iter_mut().find(|(known, _)| known == slot).ok_or("no such slot")?.1 = path.clone();
            }
            Ok(files.iter().map(|(slot, path)| format!("{slot}={path}\n")).collect::<String>().into_bytes())
        }
    }

    /// A song with two audio clips of one file and a MIDI clip on one
    /// track, a rack of two pads on another, and a sampler on the master.
    fn song() -> (Project, [ClipId; 3], [DeviceId; 2]) {
        let mut project = Project::new("Song");
        project.set_tempo(120.0).unwrap();
        let fx = project.add_track("FX");
        let riser = AudioContent { offset_seconds: 0.5, ..AudioContent::new("A/fx/riser.wav") };
        let first = project.add_audio_clip(fx, "Riser", 0.0, 4.0, riser).unwrap();
        let second = project.add_audio_clip(fx, "Riser again", 16.0, 2.0, AudioContent::new("A/fx/riser.wav")).unwrap();
        let notes = project.add_midi_clip(fx, "Notes", 32.0, 4.0).unwrap();
        let kit = project.add_track("Kit");
        let rack = project.add_device(ChainRef::Track(kit), "test:rack", "Rack", None).unwrap();
        project.device_mut(rack).unwrap().state = Some(b"pad 36=A/kit/kick.wav\npad 38=A/kit/snare.wav\n".to_vec());
        let sampler = project.add_device(ChainRef::Master, "test:sampler", "Sampler", None).unwrap();
        project.device_mut(sampler).unwrap().state = Some(b"sample=A/keys/c3.wav\n".to_vec());
        (project, [first, second, notes], [rack, sampler])
    }

    fn clip(id: ClipId, path: &str) -> Relink {
        Relink { place: FilePlace::Clip(id), path: path.into() }
    }

    fn sample(device: DeviceId, slot: &str, path: &str) -> Relink {
        Relink { place: FilePlace::Sample { device, slot: slot.into() }, path: path.into() }
    }

    #[test]
    fn a_song_lists_every_place_it_names_a_file() {
        let (project, [first, second, _], [rack, sampler]) = song();
        let uses = project.files(&Lines);
        let said: Vec<(String, &str, Option<&str>)> = uses
            .iter()
            .map(|file| {
                let track = file.track.map(|id| project.track(id).unwrap().name.as_str());
                (file.place.to_string(), file.path.as_str(), track)
            })
            .collect();
        assert_eq!(
            said,
            [
                (first.to_string(), "A/fx/riser.wav", Some("FX")),
                (second.to_string(), "A/fx/riser.wav", Some("FX")),
                (format!("pad 36 of {rack}"), "A/kit/kick.wav", Some("Kit")),
                (format!("pad 38 of {rack}"), "A/kit/snare.wav", Some("Kit")),
                (format!("sample of {sampler}"), "A/keys/c3.wav", None),
            ]
        );
        assert_eq!((uses[0].name.as_str(), uses[2].name.as_str()), ("Riser", "Rack"));
        // How far into its file a clip plays: half a second in, then four
        // beats at 120, which are two seconds.
        assert_eq!(uses[0].reach_seconds, Some(2.5));
        assert_eq!(uses[1].reach_seconds, Some(1.0));
        assert_eq!(uses[2].reach_seconds, None);
        // Without anyone who knows the devices, the clips are all there is.
        assert_eq!(project.files(&NoDeviceFiles).len(), 2);
    }

    #[test]
    fn relinking_changes_the_files_and_nothing_else() {
        let (mut project, [first, second, _], [rack, sampler]) = song();
        let before = project.clone();
        let relinks = [
            clip(first, "B/fx/riser.wav"),
            clip(second, "B/fx/riser.wav"),
            sample(rack, "pad 38", "B/kit/snare.wav"),
            sample(sampler, "sample", "B/keys/c3.wav"),
        ];
        assert_eq!(project.relink(&relinks, &Lines), Ok(4));

        let paths: Vec<String> = project.files(&Lines).into_iter().map(|file| file.path).collect();
        assert_eq!(paths, ["B/fx/riser.wav", "B/fx/riser.wav", "A/kit/kick.wav", "B/kit/snare.wav", "B/keys/c3.wav"]);
        // A clip still plays the stretch of its file it played.
        let (was, now) = (before.clip(first).unwrap(), project.clip(first).unwrap());
        let moved = AudioContent { path: "B/fx/riser.wav".into(), ..was.audio().unwrap().clone() };
        assert_eq!(now.audio(), Some(&moved));
        assert_eq!((now.start, now.length, &now.name), (was.start, was.length, &was.name));
        // And a device is the device it was, with another state.
        let (was, now) = (before.device(rack).unwrap(), project.device(rack).unwrap());
        assert_eq!((now.params.clone(), &now.name, now.enabled), (was.params.clone(), &was.name, was.enabled));

        // A place given the file it has is not a change, and neither is
        // nothing at all.
        let again = project.clone();
        assert_eq!(project.relink(&relinks, &Lines), Ok(0));
        assert_eq!(project.relink(&[], &Lines), Ok(0));
        assert_eq!(project, again);
        // A place named twice takes the path given last.
        assert_eq!(project.relink(&[clip(first, "C/x.wav"), clip(first, "C/y.wav")], &Lines), Ok(1));
        assert_eq!(project.clip(first).unwrap().audio().unwrap().path, "C/y.wav");
    }

    #[test]
    fn a_relink_that_cannot_be_done_changes_nothing_and_says_what_is_wrong() {
        let (mut project, [first, _, notes], [rack, sampler]) = song();
        let before = project.clone();
        let good = clip(first, "B/fx/riser.wav");
        let refused: [(Relink, &str); 8] = [
            (clip(ClipId(999), "B/x.wav"), "clip 999 not found"),
            (clip(notes, "B/x.wav"), "is a MIDI clip"),
            (clip(first, "  "), "the path is empty"),
            (sample(DeviceId(999), "sample", "B/x.wav"), "device 999 not found"),
            (sample(rack, "pad 40", "B/x.wav"), "has no file in 'pad 40'; its files are in: pad 36, pad 38"),
            (sample(rack, "pad 36", "B/notes.txt"), "a text file is not a sample"),
            (sample(sampler, "sample", ""), "the path is empty"),
            (sample(rack, "sample", "B/x.wav"), "has no file in 'sample'"),
        ];
        for (relink, expected) in refused {
            // The one that is wrong comes after one that is right.
            let error = project.relink(&[good.clone(), relink], &Lines).unwrap_err().to_string();
            assert!(error.contains(expected), "'{error}' should say '{expected}'");
            assert_eq!(project, before, "nothing is changed by a relink that fails: {expected}");
        }
        // A device that plays no files has no slot to fill.
        let synth = project.add_device(ChainRef::Master, "test:synth", "Synth", None).unwrap();
        let error = project.relink(&[sample(synth, "sample", "B/x.wav")], &Lines).unwrap_err().to_string();
        assert!(error.contains("plays no files"), "{error}");
        let error = project.relink(&[sample(rack, "pad 36", "B/x.wav")], &NoDeviceFiles).unwrap_err().to_string();
        assert!(error.contains("plays no files"), "{error}");
    }

    #[test]
    fn a_warped_clip_keeps_its_warp_when_its_file_is_found_elsewhere() {
        let mut project = Project::new("Song");
        let track = project.add_track("Loops");
        let content = AudioContent {
            warp: WarpMode::Beats,
            source_tempo: Some(140.0),
            gain_db: -6.0,
            fade_in: 0.5,
            transpose: 2.0,
            ..AudioContent::new("A/loop.wav")
        };
        let id = project.add_audio_clip(track, "Loop", 8.0, 8.0, content.clone()).unwrap();
        project.relink(&[clip(id, "B/loop.wav")], &NoDeviceFiles).unwrap();
        assert_eq!(project.clip(id).unwrap().audio(), Some(&AudioContent { path: "B/loop.wav".into(), ..content }));
    }

    #[test]
    fn a_folder_that_moved_is_read_from_one_file_and_takes_the_others_along() {
        let was = r"C:\Music\packs\Kit One\Kicks\Kick 01.wav";
        let is = r"C:\Music\samples\packs\Kit One\Kicks\Kick 01.wav";
        let (moved, alike) = FolderMove::between(was, is).unwrap();
        assert_eq!(
            moved,
            FolderMove::new(r"C:\Music\packs", r"C:\Music\samples\packs"),
            "the topmost folder that kept its name"
        );
        assert_eq!(alike, 4);
        assert_eq!(moved.apply(was).as_deref(), Some(is));
        assert_eq!(
            moved.apply(r"C:\Music\packs\Loops\Bass 120.wav").as_deref(),
            Some(r"C:\Music\samples\packs\Loops\Bass 120.wav")
        );
        assert_eq!(moved.apply(r"C:\Music\packs2\x.wav"), None, "a folder whose name merely starts the same");
        assert_eq!(moved.apply(r"C:\Music\other\x.wav"), None);
        assert_eq!(moved.apply(r"C:\Music\packs"), None, "the folder itself is not a file in it");
        // A path written with the other slash is the same path.
        assert_eq!(
            moved.apply("C:/Music/packs/Loops/Bass 120.wav").as_deref(),
            Some(r"C:\Music\samples\packs\Loops\Bass 120.wav")
        );

        // Only the name in common: the folder the file was in went to the
        // folder it is in.
        let (renamed, alike) = FolderMove::between("/old/Samples/kick.wav", "/new/Drums/kick.wav").unwrap();
        assert_eq!((renamed, alike), (FolderMove::new("/old/Samples", "/new/Drums"), 1));
        // A folder that was moved and renamed, seen from two of its
        // folders, is the same move one up.
        let (inner, _) = FolderMove::between("/a/packs/drums/k.wav", "/b/packs2/drums/k.wav").unwrap();
        assert_eq!(inner, FolderMove::new("/a/packs/drums", "/b/packs2/drums"));
        assert_eq!(inner.parent(), Some(FolderMove::new("/a/packs", "/b/packs2")));
        assert_eq!(FolderMove::new("/a/x", "/b/y").parent(), None, "nothing says these two belong together");

        // Not the same file at all, the same path, or no folder to speak of.
        assert_eq!(FolderMove::between("/a/kick.wav", "/b/snare.wav"), None);
        assert_eq!(FolderMove::between("/a/kick.wav", "/a/kick.wav"), None);
        assert_eq!(FolderMove::between("kick.wav", "/b/kick.wav"), None);
        // A file that went one folder down, into a folder of the same name.
        let (nested, _) = FolderMove::between("/a/kit/k.wav", "/a/kit/kit/k.wav").unwrap();
        assert_eq!(nested.apply("/a/kit/s.wav").as_deref(), Some("/a/kit/kit/s.wav"));
    }

    #[test]
    fn names_are_compared_the_way_the_file_system_compares_them() {
        assert!(same_name_folding("Kick.WAV", "kick.wav", true));
        assert!(!same_name_folding("Kick.WAV", "kick.wav", false));
        assert!(same_name_folding("Übung", "übung", true));
        assert_eq!(file_name(r"C:\a\b\Kick 01.wav"), "Kick 01.wav");
        assert_eq!(file_name("a/b/"), "b");
        assert_eq!(file_name("kick.wav"), "kick.wav");
        assert_eq!(folder_of(r"C:\a\b\Kick 01.wav"), r"C:\a\b");
        assert_eq!(folder_of("/a/b/"), "/a");
        assert_eq!(folder_of("kick.wav"), "");
        assert_eq!(below("/a/b", "/a/b/c/d.wav"), Some("c/d.wav"));
        assert_eq!(below("/a/b/", "/a/b/c.wav"), Some("c.wav"));
        assert_eq!(below("/a/b", "/a/bc/d.wav"), None);
        assert_eq!(below("/a/b", "/a/b"), None);
        assert_eq!(below("/a/b/c", "/a/b"), None);
        assert!(same_path(r"C:\a\b\k.wav", "C:/a/b/k.wav") && same_path("/a/b/", "/a/b"));
        assert!(!same_path("/a/b/k.wav", "/a/c/k.wav") && !same_path("/a/b", "/a/b/c"));
        assert_eq!(alike("A/kit/kicks/k.wav", "B/kit/kicks/k.wav"), 3);
        assert_eq!(alike("A/kit/kicks/k.wav", "B/k.wav"), 1);
        assert_eq!(alike("A/kit/kick.wav", "A/kit/snare.wav"), 0);
    }

    #[test]
    fn a_file_is_said_from_the_songs_folder_and_found_again_from_there() {
        let cases = [
            ("D:/Songs/Night", "D:/Songs/Night/samples/kick.wav", "samples/kick.wav"),
            ("D:/Songs/Night", "D:/Songs/Night/kick.wav", "kick.wav"),
            ("D:/Songs/Night", "D:/Packs/Kit/kick.wav", "../../Packs/Kit/kick.wav"),
            (r"C:\Users\A\Documents\Trill", r"C:\Users\A\Music\packs\k.wav", "../../Music/packs/k.wav"),
            ("/home/a/songs", "/home/a/songs/s/k.wav", "s/k.wav"),
            ("/home/a/songs/", "/mnt/packs/k.wav", "../../../mnt/packs/k.wav"),
        ];
        for (folder, path, beside) in cases {
            assert_eq!(relative_to(folder, path).as_deref(), Some(beside), "{path} from {folder}");
            // And back: the same path, written the way the folder is.
            let back = resolve(folder, beside).unwrap();
            assert_eq!(parts(&back), parts(path), "{beside} from {folder}");
        }
        // The song's folder went elsewhere with its samples: the same way
        // leads to them from there.
        assert_eq!(resolve(r"E:\Sent\Night", "samples/kick.wav").as_deref(), Some(r"E:\Sent\Night\samples\kick.wav"));
        assert_eq!(resolve("/x/y", "../z/k.wav").as_deref(), Some("/x/z/k.wav"));

        // No way from one drive to another, or from a path that is not whole.
        assert_eq!(relative_to("D:/Songs", "E:/Packs/k.wav"), None);
        assert_eq!(relative_to("D:/Songs", "packs/k.wav"), None);
        assert_eq!(relative_to("songs", "D:/Packs/k.wav"), None);
        assert_eq!(relative_to("D:/Songs", "/Packs/k.wav"), None);
        assert_eq!(relative_to("D:/Songs", "D:/Songs"), None, "the folder itself is not a file");
        assert_eq!(relative_to(r"\\nas\music\songs", r"\\nas\music\packs\k.wav").as_deref(), Some("../packs/k.wav"));
        assert_eq!(relative_to(r"\\nas\music\songs", r"\\other\music\packs\k.wav"), None, "another machine");
        // Nor above the top.
        assert_eq!(resolve("D:/Songs", "../../k.wav"), None);
        assert_eq!(resolve("/a", "../../k.wav"), None);
    }

    fn known(path: &str, bytes: u64, frames: u64) -> KnownFile {
        KnownFile {
            path: path.into(),
            beside: Some(file_name(path).to_owned()),
            bytes: Some(bytes),
            frames: Some(frames),
            sample_rate: Some(44_100),
            channels: Some(2),
        }
    }

    #[test]
    fn what_was_written_down_about_a_file_tells_it_from_one_that_has_its_name() {
        let kick = known("A/kick.wav", 88_244, 22_050);
        assert_eq!(kick.is(&known("B/kick.wav", 88_244, 22_050)), Some(true), "wherever it is");
        assert_eq!(kick.is(&known("B/kick.wav", 90_000, 22_050)), Some(false));
        assert_eq!(kick.is(&known("B/kick.wav", 88_244, 30_000)), Some(false));
        assert_eq!(kick.is(&KnownFile { sample_rate: Some(48_000), ..kick.clone() }), Some(false));
        assert_eq!(kick.is(&KnownFile { channels: Some(1), ..kick.clone() }), Some(false));
        assert_eq!(kick.seconds(), Some(0.5));

        // Nothing written down, nothing to go by.
        let bare = KnownFile { path: "A/kick.wav".into(), ..KnownFile::default() };
        assert!(!bare.says_what_it_was() && kick.says_what_it_was());
        assert_eq!(bare.is(&kick), None);
        assert_eq!(kick.is(&bare), None);
        assert_eq!(bare.seconds(), None);
        // What both say is what counts: a compressed file states no length.
        let no_length = KnownFile { frames: None, ..kick.clone() };
        assert_eq!(kick.is(&no_length), Some(true));
    }

    #[test]
    fn a_project_file_carries_its_table_and_a_file_without_one_is_what_it_was() {
        let (project, ..) = song();
        let table =
            FileTable::new([known("A/kit/kick.wav", 88_244, 22_050), known("A/fx/riser.wav", 705_644, 176_400)]);
        assert_eq!(table.files()[0].path, "A/fx/riser.wav", "in the order of the paths");
        assert_eq!(table.get("A/kit/kick.wav").and_then(|file| file.bytes), Some(88_244));
        assert_eq!(table.get("A/kit/snare.wav"), None);

        let text = write(&project, &table);
        assert_eq!(read(&text), Ok((project.clone(), table.clone())));
        // The song in the file is the song, in the place it always was:
        // whoever reads only the song reads it as before.
        assert_eq!(Project::from_json(&text), Ok(project.clone()));
        let begins = format!("{{\n  \"format_version\": {},\n  \"name\": \"Song\"", crate::project::FORMAT_VERSION);
        assert!(text.starts_with(&begins), "{}", &text[..60]);
        assert!(text.contains("\"beside\": \"kick.wav\""));

        // Without a table the file is to the letter what it has always been.
        assert_eq!(write(&project, &FileTable::default()), project.to_json());
        assert_eq!(read(&project.to_json()), Ok((project.clone(), FileTable::default())));
        // A table that makes no sense costs the table, not the song.
        let odd = project.to_json().replacen('{', "{\n  \"files\": \"all of them\",", 1);
        assert_eq!(read(&odd), Ok((project.clone(), FileTable::default())));
        let nameless =
            project.to_json().replacen('{', "{\n  \"files\": [{ \"path\": \"\" }, { \"path\": \"x.wav\" }],", 1);
        assert_eq!(read(&nameless).unwrap().1.files().len(), 1);
        assert!(read("not a project").is_err());

        // What is known of a file is known of it where it was found.
        let mut table = table;
        table.found_at("A/kit/kick.wav", "B/kit/kick.wav");
        assert_eq!(table.get("A/kit/kick.wav").and_then(|file| file.bytes), Some(88_244), "a step can be taken back");
        let moved = table.get("B/kit/kick.wav").unwrap();
        assert_eq!(
            (moved.bytes, moved.beside.as_deref()),
            (Some(88_244), None),
            "where it is from the song is asked anew"
        );
        table.found_at("A/nothing.wav", "B/nothing.wav");
        assert_eq!(table.files().len(), 3);
    }
}
