//! The index: every file of the library, arranged as the folder tree it
//! was found in, with the text and facts searches run over laid out for
//! speed.
//!
//! An index never changes once built. A scan builds a new one and the
//! library swaps it in, so readers hold an `Arc` and are never blocked or
//! surprised by a change halfway through drawing a list.

use crate::entry::{Entry, EntryKind};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Identifies an [`Entry`] within one [`Index`]: its position in
/// [`Index::entries`]. Ids are not stable across scans; remember a file
/// across them by its path and look it up with [`Index::find`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub(crate) u32);

impl EntryId {
    /// Position in [`Index::entries`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Identifies a [`Folder`] within one [`Index`]. Like [`EntryId`], valid
/// only for the index it came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderId(pub(crate) u32);

/// A folder of the library: a root, or a folder beneath one that has
/// library files somewhere inside it. Folders with nothing to list are left
/// out.
#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    path: PathBuf,
    name: String,
    parent: Option<FolderId>,
    subfolders: Vec<FolderId>,
    /// The files directly in this folder.
    files: Range<u32>,
    /// One past the last entry anywhere beneath this folder. Entries are
    /// stored in tree order, so the subtree is `files.start..subtree_end`.
    subtree_end: u32,
}

impl Folder {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder's own name, as shown in a tree.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The folder this one is in; `None` for a library root.
    pub fn parent(&self) -> Option<FolderId> {
        self.parent
    }

    /// Folders directly inside this one, in listing order.
    pub fn subfolders(&self) -> &[FolderId] {
        &self.subfolders
    }

    /// The files directly in this folder, in listing order, as positions in
    /// [`Index::entries`].
    pub fn files(&self) -> Range<usize> {
        self.files.start as usize..self.files.end as usize
    }

    /// Every file in this folder and the folders beneath it, as positions
    /// in [`Index::entries`].
    pub fn all_files(&self) -> Range<usize> {
        self.files.start as usize..self.subtree_end as usize
    }
}

/// The facts filters test, packed so that a search touches a few megabytes
/// rather than every [`Entry`].
#[derive(Copy, Clone)]
pub(crate) struct Facts {
    pub tags: u64,
    /// Beats per minute; NaN when unknown.
    pub tempo: f32,
    /// Length in seconds; NaN when unknown.
    pub seconds: f32,
    pub kind: u8,
    /// [`crate::Key::code`]; [`Facts::NO_KEY`] when unknown.
    pub key: u8,
    pub unreadable: bool,
}

impl Facts {
    pub(crate) const NO_KEY: u8 = u8::MAX;

    fn of(entry: &Entry) -> Self {
        Self {
            tags: entry.tags.bits(),
            tempo: entry.tempo.unwrap_or(f32::NAN),
            seconds: entry.seconds().map_or(f32::NAN, |seconds| seconds as f32),
            kind: entry.kind.code(),
            key: entry.key.map_or(Self::NO_KEY, |key| key.code()),
            unreadable: entry.unreadable,
        }
    }
}

/// Where in an item of a [`Haystack`] a match begins.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MatchPlace {
    /// In the middle of a word.
    Inside,
    /// At the start of a word.
    WordStart,
    /// At the very start of the item.
    Start,
}

/// Many short texts stored as one, so that a search is a single pass over
/// contiguous memory instead of a hundred thousand small ones.
pub(crate) struct Haystack {
    /// The items in lower case, each followed by a line break.
    text: Vec<u8>,
    /// Where each item starts in `text`, plus the end of the last.
    starts: Vec<u32>,
    /// One bit per byte of `text`: set where a word starts.
    word_starts: Vec<u64>,
}

impl Default for Haystack {
    fn default() -> Self {
        // `starts` always holds one more offset than there are items.
        Self { text: Vec::new(), starts: vec![0], word_starts: Vec::new() }
    }
}

impl Haystack {
    /// Adds an item. Words start where [`crate::infer::words`] splits a
    /// name: at the first character, after anything that is not a letter or
    /// digit, where letters meet digits, and at the humps of camelCase
    /// (`DubKick`, and the `L` of `FXLoop`).
    fn push(&mut self, item: &str) {
        let start = self.text.len();
        if item.is_ascii() {
            // Nearly every file name; worth not going through `char`.
            let bytes = item.as_bytes();
            for (offset, &byte) in bytes.iter().enumerate() {
                let starts_word = offset == 0 || {
                    let previous = bytes[offset - 1];
                    let next_is_lower = bytes.get(offset + 1).is_some_and(u8::is_ascii_lowercase);
                    byte.is_ascii_alphanumeric()
                        && (!previous.is_ascii_alphanumeric()
                            || previous.is_ascii_digit() != byte.is_ascii_digit()
                            || (previous.is_ascii_lowercase() && byte.is_ascii_uppercase())
                            || (previous.is_ascii_uppercase() && byte.is_ascii_uppercase() && next_is_lower))
                };
                if starts_word {
                    self.mark_word_start(start + offset);
                }
            }
            self.text.extend(bytes.iter().map(u8::to_ascii_lowercase));
        } else {
            let mut previous: Option<char> = None;
            let mut chars = item.chars().peekable();
            while let Some(c) = chars.next() {
                let starts_word = match previous {
                    None => true,
                    Some(previous) => {
                        let next_is_lower = chars.peek().is_some_and(|next| next.is_lowercase());
                        c.is_alphanumeric()
                            && (!previous.is_alphanumeric()
                                || previous.is_ascii_digit() != c.is_ascii_digit()
                                || (previous.is_lowercase() && c.is_uppercase())
                                || (previous.is_uppercase() && c.is_uppercase() && next_is_lower))
                    }
                };
                if starts_word {
                    self.mark_word_start(self.text.len());
                }
                let mut buffer = [0; 4];
                for lower in c.to_lowercase() {
                    self.text.extend_from_slice(lower.encode_utf8(&mut buffer).as_bytes());
                }
                previous = Some(c);
            }
        }
        self.text.push(b'\n');
        self.starts.push(u32::try_from(self.text.len()).unwrap_or(u32::MAX));
    }

    fn mark_word_start(&mut self, position: usize) {
        if self.word_starts.len() <= position / 64 {
            self.word_starts.resize(position / 64 + 1, 0);
        }
        self.word_starts[position / 64] |= 1 << (position % 64);
    }

    pub(crate) fn len(&self) -> usize {
        self.starts.len() - 1
    }

    fn is_word_start(&self, position: usize) -> bool {
        self.word_starts.get(position / 64).is_some_and(|bits| bits & (1 << (position % 64)) != 0)
    }

    /// Finds `needle` (lower case, no line breaks) in the items `items` and
    /// reports, for every item it occurs in, the best place it occurs at.
    pub(crate) fn find(&self, needle: &[u8], items: Range<usize>, mut found: impl FnMut(usize, MatchPlace)) {
        let items = items.start.min(self.len())..items.end.min(self.len());
        if needle.is_empty() || items.is_empty() {
            return;
        }
        let end = self.starts[items.end] as usize;
        let finder = memchr::memmem::Finder::new(needle);
        let mut item = items.start;
        let mut from = self.starts[item] as usize;
        while let Some(offset) = finder.find(&self.text[from..end]) {
            let position = from + offset;
            while self.starts[item + 1] as usize <= position {
                item += 1;
            }
            let (item_start, item_end) = (self.starts[item] as usize, self.starts[item + 1] as usize);
            let mut place = if position == item_start { MatchPlace::Start } else { MatchPlace::Inside };
            // An occurrence inside a word may be followed, in the same
            // item, by one that starts a word.
            let mut at = position;
            while place == MatchPlace::Inside {
                if self.is_word_start(at) {
                    place = MatchPlace::WordStart;
                } else {
                    match finder.find(&self.text[at + 1..item_end]) {
                        Some(offset) => at += 1 + offset,
                        None => break,
                    }
                }
            }
            found(item, place);
            // Nothing later in this item can do better; go on to the next.
            item += 1;
            if item >= items.end {
                break;
            }
            from = self.starts[item] as usize;
        }
    }
}

/// Every file of the library and the folder tree they were found in.
#[derive(Default)]
pub struct Index {
    pub(crate) folders: Vec<Folder>,
    roots: Vec<FolderId>,
    pub(crate) entries: Vec<Entry>,
    /// The folder each entry is directly in.
    pub(crate) folder_of: Vec<u32>,
    pub(crate) facts: Vec<Facts>,
    /// File names, extension included, one item per entry.
    pub(crate) names: Haystack,
    /// For each folder, the names of its root and of the folders leading
    /// down to it, joined by `/`.
    pub(crate) folder_paths: Haystack,
    folder_by_path: HashMap<PathBuf, FolderId>,
}

impl Index {
    /// Number of files.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every file, in tree order: each folder's own files in listing order,
    /// followed by those of its subfolders.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry `id` names, or `None` if the id came from another index.
    pub fn entry(&self, id: EntryId) -> Option<&Entry> {
        self.entries.get(id.index())
    }

    /// The entry for the file at `path`, spelled as the library lists it.
    pub fn find(&self, path: &Path) -> Option<EntryId> {
        let folder = self.folder(self.find_folder(path.parent()?)?)?;
        let position = self.entries[folder.files()].iter().position(|entry| entry.path == path)?;
        Some(EntryId(folder.files.start + position as u32))
    }

    /// The library roots that have been scanned, in the order they were
    /// added.
    pub fn roots(&self) -> &[FolderId] {
        &self.roots
    }

    /// The folder `id` names, or `None` if the id came from another index.
    pub fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.get(id.0 as usize)
    }

    /// The folder at `path`, spelled as the library lists it.
    pub fn find_folder(&self, path: &Path) -> Option<FolderId> {
        self.folder_by_path.get(path).copied()
    }

    /// The folder `entry` is directly in.
    pub fn folder_of(&self, id: EntryId) -> Option<FolderId> {
        self.folder_of.get(id.index()).map(|folder| FolderId(*folder))
    }

    /// Number of files of each kind, for a summary line.
    pub fn count(&self, kind: EntryKind) -> usize {
        self.facts.iter().filter(|facts| facts.kind == kind.code()).count()
    }

    /// Copies the root `root` and everything beneath it into `builder`.
    pub(crate) fn copy_root(&self, root: FolderId, builder: &mut IndexBuilder) -> Result<(), OutOfOrder> {
        // Folders are stored in tree order, so a root's tree runs up to the
        // next root.
        let start = root.0 as usize;
        let end =
            self.roots.iter().map(|next| next.0 as usize).find(|next| *next > start).unwrap_or(self.folders.len());
        let mut copied: Vec<FolderId> = Vec::with_capacity(end.saturating_sub(start));
        for folder in self.folders.get(start..end).ok_or(OutOfOrder)? {
            let parent = match folder.parent {
                Some(parent) => {
                    Some(*(parent.0 as usize).checked_sub(start).and_then(|at| copied.get(at)).ok_or(OutOfOrder)?)
                }
                None => None,
            };
            copied.push(builder.folder(folder.path.clone(), parent)?);
            for entry in &self.entries[folder.files()] {
                builder.file(entry.clone())?;
            }
        }
        Ok(())
    }

    /// A copy of this index holding only the roots `keep` accepts.
    pub(crate) fn retain_roots(&self, keep: impl Fn(&Path) -> bool) -> Index {
        let mut builder = IndexBuilder::with_capacity(self.len());
        for root in &self.roots {
            let kept = self.folder(*root).is_some_and(|folder| keep(folder.path()));
            if kept && self.copy_root(*root, &mut builder).is_err() {
                // Unreachable for an index this module built; an empty
                // index is rebuilt by the next scan.
                return Index::default();
            }
        }
        builder.finish()
    }
}

/// The reason a tree could not be built: its folders were not given parent
/// first. Only a damaged index file can cause it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OutOfOrder;

/// Builds an [`Index`] from a tree given in listing order: each folder,
/// then its files, then its subfolders in turn.
#[derive(Default)]
pub(crate) struct IndexBuilder {
    folders: Vec<Folder>,
    roots: Vec<FolderId>,
    entries: Vec<Entry>,
    folder_of: Vec<u32>,
    /// The folder being filled and its ancestors, outermost first.
    open: Vec<u32>,
}

impl IndexBuilder {
    pub(crate) fn with_capacity(entries: usize) -> Self {
        Self { entries: Vec::with_capacity(entries), folder_of: Vec::with_capacity(entries), ..Self::default() }
    }

    /// Starts a folder. `parent` is `None` for a root; otherwise it must be
    /// the folder started last or one of the folders that one is inside.
    pub(crate) fn folder(&mut self, path: PathBuf, parent: Option<FolderId>) -> Result<FolderId, OutOfOrder> {
        let depth = match parent {
            None => 0,
            Some(parent) => self.open.iter().position(|open| *open == parent.0).ok_or(OutOfOrder)? + 1,
        };
        self.close_to(depth);
        let id = u32::try_from(self.folders.len()).map_err(|_| OutOfOrder)?;
        let name = path.file_name().map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy()).into_owned();
        let at = self.entries.len() as u32;
        self.folders.push(Folder { path, name, parent, subfolders: Vec::new(), files: at..at, subtree_end: at });
        match parent {
            Some(parent) => self.folders[parent.0 as usize].subfolders.push(FolderId(id)),
            None => self.roots.push(FolderId(id)),
        }
        self.open.push(id);
        Ok(FolderId(id))
    }

    /// Adds a file to the folder started last.
    pub(crate) fn file(&mut self, entry: Entry) -> Result<(), OutOfOrder> {
        let folder = *self.open.last().ok_or(OutOfOrder)?;
        // A folder's own files come before its subfolders.
        if folder as usize != self.folders.len() - 1 || self.entries.len() >= u32::MAX as usize {
            return Err(OutOfOrder);
        }
        self.entries.push(entry);
        self.folder_of.push(folder);
        self.folders[folder as usize].files.end = self.entries.len() as u32;
        Ok(())
    }

    /// Finishes the folders deeper than `depth`.
    fn close_to(&mut self, depth: usize) {
        while self.open.len() > depth {
            if let Some(folder) = self.open.pop() {
                self.folders[folder as usize].subtree_end = self.entries.len() as u32;
            }
        }
    }

    pub(crate) fn finish(mut self) -> Index {
        self.close_to(0);
        let mut names = Haystack::default();
        for entry in &self.entries {
            match entry.path.file_name() {
                Some(name) => names.push(&name.to_string_lossy()),
                None => names.push(&entry.name),
            }
        }
        // A folder is searched by the names from its root down, not by the
        // path above the root: "Users" is not something to find sounds by.
        let mut folder_paths = Haystack::default();
        let mut labels: Vec<String> = Vec::with_capacity(self.folders.len());
        for folder in &self.folders {
            let label = match folder.parent {
                Some(parent) => format!("{}/{}", labels[parent.0 as usize], folder.name),
                None => folder.name.clone(),
            };
            folder_paths.push(&label);
            labels.push(label);
        }
        let folder_by_path =
            self.folders.iter().enumerate().map(|(id, folder)| (folder.path.clone(), FolderId(id as u32))).collect();
        Index {
            facts: self.entries.iter().map(Facts::of).collect(),
            folders: self.folders,
            roots: self.roots,
            entries: self.entries,
            folder_of: self.folder_of,
            names,
            folder_paths,
            folder_by_path,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::entry::display_name;
    use crate::tags::Tags;

    /// An audio entry with nothing known about it but its path.
    pub(crate) fn entry(path: &str) -> Entry {
        let path = PathBuf::from(path);
        Entry {
            name: display_name(&path),
            kind: EntryKind::of(&path).unwrap_or(EntryKind::Audio),
            path,
            size: 0,
            modified: None,
            audio: None,
            unreadable: false,
            tempo: None,
            key: None,
            tags: Tags::EMPTY,
        }
    }

    /// Builds an index from paths given in tree order. The first component
    /// of each path is its root.
    pub(crate) fn index_of(entries: Vec<Entry>) -> Index {
        let mut builder = IndexBuilder::default();
        let mut ids: HashMap<PathBuf, FolderId> = HashMap::new();
        for entry in entries {
            let folder = entry.path.parent().unwrap().to_path_buf();
            if !ids.contains_key(&folder) {
                // Start every folder on the way down that is not there yet.
                let mut missing: Vec<&Path> =
                    folder.ancestors().take_while(|path| !path.as_os_str().is_empty()).collect();
                missing.reverse();
                for path in missing {
                    if !ids.contains_key(path) {
                        let parent = path.parent().and_then(|parent| ids.get(parent)).copied();
                        ids.insert(path.to_path_buf(), builder.folder(path.to_path_buf(), parent).unwrap());
                    }
                }
            }
            builder.file(entry).unwrap();
        }
        builder.finish()
    }

    fn pack() -> Index {
        index_of(
            [
                "Pack/readme.mid",
                "Pack/Drums/Kicks/Kick 1.wav",
                "Pack/Drums/Kicks/Kick 2.wav",
                "Pack/Drums/Snares/Snare.wav",
                "Pack/Loops/Loop 140.wav",
                "Other/Pad.flac",
            ]
            .into_iter()
            .map(entry)
            .collect(),
        )
    }

    #[test]
    fn the_tree_can_be_walked_and_searched_by_path() {
        let index = pack();
        assert_eq!((index.len(), index.is_empty()), (6, false));
        let names =
            |ids: &[FolderId]| ids.iter().map(|id| index.folder(*id).unwrap().name().to_string()).collect::<Vec<_>>();
        assert_eq!(names(index.roots()), ["Pack", "Other"]);

        let pack = index.folder(index.roots()[0]).unwrap();
        assert_eq!(pack.path(), Path::new("Pack"));
        assert_eq!(pack.parent(), None);
        assert_eq!(names(pack.subfolders()), ["Drums", "Loops"]);
        assert_eq!(index.entries()[pack.files()].iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["readme"]);
        assert_eq!(pack.all_files(), 0..5);

        let drums = index.folder(index.find_folder(Path::new("Pack/Drums")).unwrap()).unwrap();
        assert!(drums.files().is_empty(), "a folder with only subfolders has no files of its own");
        assert_eq!(drums.all_files().len(), 3);
        assert_eq!(drums.parent(), Some(index.roots()[0]));
        assert_eq!(names(drums.subfolders()), ["Kicks", "Snares"]);

        let kick = index.find(Path::new("Pack/Drums/Kicks/Kick 2.wav")).unwrap();
        assert_eq!(index.entry(kick).unwrap().name, "Kick 2");
        assert_eq!(index.folder_of(kick), index.find_folder(Path::new("Pack/Drums/Kicks")));
        assert_eq!(index.find(Path::new("Pack/Drums/Kicks/Kick 3.wav")), None);
        assert_eq!(index.find(Path::new("Nowhere/Kick 1.wav")), None);
        assert_eq!(index.find(Path::new("")), None);
        assert_eq!(index.entry(EntryId(99)), None);
        assert_eq!(index.folder(FolderId(99)), None);
        assert_eq!((index.count(EntryKind::Audio), index.count(EntryKind::Midi)), (5, 1));
    }

    #[test]
    fn roots_can_be_dropped_and_copied() {
        let index = pack();
        let other_only = index.retain_roots(|root| root == Path::new("Other"));
        assert_eq!(other_only.len(), 1);
        assert_eq!(other_only.roots().len(), 1);
        assert_eq!(other_only.find(Path::new("Other/Pad.flac")), Some(EntryId(0)));
        assert_eq!(other_only.find(Path::new("Pack/readme.mid")), None);

        let everything = index.retain_roots(|_| true);
        assert_eq!(everything.entries(), index.entries());
        assert_eq!(everything.folders, index.folders);
        assert!(index.retain_roots(|_| false).is_empty());
    }

    #[test]
    fn folders_must_come_parent_first() {
        let mut builder = IndexBuilder::default();
        assert_eq!(builder.file(entry("a/x.wav")), Err(OutOfOrder), "no folder yet");
        let root = builder.folder("a".into(), None).unwrap();
        let child = builder.folder("a/b".into(), Some(root)).unwrap();
        builder.folder("a/c".into(), Some(root)).unwrap();
        assert_eq!(builder.folder("a/b/d".into(), Some(child)), Err(OutOfOrder), "b was already finished");
        assert_eq!(builder.folder("z".into(), Some(FolderId(40))), Err(OutOfOrder));
        // An empty tree is a valid, empty index.
        let index = IndexBuilder::default().finish();
        assert!(index.is_empty() && index.roots().is_empty());
    }

    fn places(haystack: &Haystack, needle: &str, items: Range<usize>) -> Vec<(usize, MatchPlace)> {
        let mut found = Vec::new();
        haystack.find(needle.as_bytes(), items, |item, place| found.push((item, place)));
        found
    }

    #[test]
    fn matches_report_the_best_place_in_each_item() {
        assert!(places(&Haystack::default(), "kick", 0..5).is_empty(), "nothing to search");
        let mut haystack = Haystack::default();
        for item in
            ["Kick Hard.wav", "Hard Kick 01.wav", "DubKick4.wav", "sidekick.wav", "Snare.wav", "contrabass Bass.wav"]
        {
            haystack.push(item);
        }
        assert_eq!(
            places(&haystack, "kick", 0..6),
            [(0, MatchPlace::Start), (1, MatchPlace::WordStart), (2, MatchPlace::WordStart), (3, MatchPlace::Inside)]
        );
        assert_eq!(places(&haystack, "bass", 0..6), [(5, MatchPlace::WordStart)], "a later, better occurrence counts");
        assert_eq!(places(&haystack, "4", 0..6), [(2, MatchPlace::WordStart)], "digits start words");
        assert_eq!(places(&haystack, "wav", 4..5), [(4, MatchPlace::WordStart)]);
        assert_eq!(places(&haystack, "kick", 1..3).len(), 2, "only the items asked for");
        assert!(places(&haystack, "kick", 4..99).is_empty());
        assert!(places(&haystack, "", 0..6).is_empty());
        assert!(places(&haystack, "kick", 3..3).is_empty());

        // Overlapping occurrences: the one that starts a word is found.
        let mut overlapping = Haystack::default();
        overlapping.push("xaa aa");
        overlapping.push("aaa");
        assert_eq!(places(&overlapping, "aa", 0..2), [(0, MatchPlace::WordStart), (1, MatchPlace::Start)]);
    }

    #[test]
    fn plain_and_accented_names_split_into_words_alike() {
        // The same name with and without a character outside ASCII takes
        // the two paths through `push`; they must agree on where words are.
        let name = "DubKick4_OpenHat-02 FXLoop.wav";
        let mut haystack = Haystack::default();
        haystack.push(name);
        haystack.push(&format!("{name}é"));
        let item_length = name.len() + 1;
        for position in 0..name.len() {
            assert_eq!(
                haystack.is_word_start(position),
                haystack.is_word_start(item_length + position),
                "at '{}'",
                &name[position..]
            );
        }
        let starts: String =
            (0..name.len()).filter(|at| haystack.is_word_start(*at)).map(|at| name.as_bytes()[at] as char).collect();
        assert_eq!(starts, "DK4OH0FLw");
        // The same places the name is split into words for tagging.
        assert_eq!(crate::infer::words(name), ["dub", "kick", "4", "open", "hat", "02", "fx", "loop", "wav"]);
    }

    #[test]
    fn text_is_folded_to_lower_case_whatever_the_script() {
        let mut haystack = Haystack::default();
        haystack.push("ÜBER Bass İstanbul");
        haystack.push("Größe");
        assert_eq!(places(&haystack, "über", 0..2), [(0, MatchPlace::Start)]);
        assert_eq!(places(&haystack, "bass", 0..2), [(0, MatchPlace::WordStart)]);
        assert_eq!(places(&haystack, "stanbul", 0..2), [(0, MatchPlace::Inside)]);
        assert_eq!(places(&haystack, "größe", 0..2), [(1, MatchPlace::Start)]);
    }
}
