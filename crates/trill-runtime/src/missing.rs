//! Files that are not where the song says: which they are, where they may
//! have gone, and putting the song right.
//!
//! A song names its audio files by their paths, and a folder that is moved
//! or renamed leaves it with clips that are silent. This is where the
//! program looks for them, which takes the session (what the song names),
//! the library (every file it knows, by name and folder) and the disk
//! together:
//!
//! * [`Runtime::files_missing`] says what is missing, and costs a look at
//!   each file. [`Runtime::missing_files`] also looks for them.
//! * A file is looked for **where its folder went** first. A folder that
//!   went elsewhere is read from any one file that is found (`A/x/y.wav`
//!   at `B/x/y.wav`: what was under `A` is under `B`,
//!   [`FolderMove::between`]), from a folder of the library that is missing
//!   and turns up nearby, from a folder of the same name near where the
//!   missing one was, and from what the caller says ([`Look`]). Then it is
//!   looked for **by its name**, in the library and in the folders the
//!   caller names.
//! * A file of the same name can be another file. Each thing found is a
//!   [`Candidate`] that says how sure it is and why ([`Confidence`]): held
//!   against what the song wrote down about its file (size, length, sample
//!   rate: `trill_core::files`), what the library had read of it, and how
//!   far into the file the clips play. Only a candidate that is
//!   [`Confidence::Sure`] is taken without being looked at
//!   ([`Missing::relinks`]); nothing is ever relinked by finding it.
//! * [`Runtime::relink`] gives the song the files, one undo step, and
//!   [`Runtime::move_library_folder`] tells the library where a folder of
//!   its own went, with its favorites.

use crate::Runtime;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use trill_core::files::{self, FilePlace, FileUse, FolderMove, KnownFile, Relink};
use trill_core::{FileTable, Project, TrackId};
use trill_engine::{Session, SessionError, file_facts};
use trill_library::{Index, MovedRoot};

/// How many folders are listed in all when looking near where a folder was
/// for one of its name. Looking is for the common case, a folder moved
/// within the folder it was in, and must not turn into reading a disk.
const NEARBY_FOLDERS: usize = 3_000;
/// How deep below a folder near the missing one the look goes.
const NEARBY_DEPTH: usize = 4;
/// How many folders are listed in all inside the folders a caller names:
/// they asked, so it is thorough, and it still ends.
const NAMED_FOLDERS: usize = 60_000;
/// How many of a missing library folder's files are looked for in a
/// folder that may be it.
const FILES_CHECKED: usize = 48;
/// How many candidates are kept for one file.
const CANDIDATES: usize = 4;
/// A clip may play this much past the end of its file before the file is
/// taken to be too short for it, in seconds.
const REACH_SLACK: f64 = 0.05;

/// Folders that are never where someone's samples went.
const PASSED_OVER: &[&str] = &[
    "AppData",
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "node_modules",
    "System Volume Information",
    "__MACOSX",
];

/// How sure it is that a file that was found is the file the song means.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// It has the name, and something speaks against it or nothing tells
    /// it from others of that name. Somebody has to look.
    Unsure,
    /// It has the name and nothing speaks against it, but nothing else
    /// says it is the file either.
    Likely,
    /// It is where the file's folder went, or it is what the song wrote
    /// down about its file, and nothing speaks against it.
    Sure,
}

impl Confidence {
    pub fn word(self) -> &'static str {
        match self {
            Confidence::Sure => "sure",
            Confidence::Likely => "likely",
            Confidence::Unsure => "unsure",
        }
    }
}

/// A file that may be the one a song is missing.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub path: String,
    pub confidence: Confidence,
    /// Why it was found and what speaks for and against it, in words.
    pub why: String,
    /// Its length in seconds, when its header says.
    pub seconds: Option<f64>,
}

/// A file the song names that is not there.
#[derive(Clone, Debug, PartialEq)]
pub struct MissingFile {
    /// The path as the song has it.
    pub path: String,
    /// Every place of the song that names it.
    pub uses: Vec<FileUse>,
    /// The names of the tracks those places are on.
    pub tracks: Vec<String>,
    /// What was written down about the file while it was there: by the
    /// song when it was saved, or else by the library.
    pub known: Option<KnownFile>,
    /// What was found for it, the best first. Empty when nothing was, and
    /// when nobody looked.
    pub candidates: Vec<Candidate>,
}

impl MissingFile {
    /// The best of what was found.
    pub fn found(&self) -> Option<&Candidate> {
        self.candidates.first()
    }

    /// The best of what was found, if it is at least `sure`.
    pub fn found_at_least(&self, sure: Confidence) -> Option<&Candidate> {
        self.found().filter(|candidate| candidate.confidence >= sure)
    }

    /// Every place that names the file, pointed at `path`.
    pub fn relinks_to(&self, path: &str) -> Vec<Relink> {
        self.uses.iter().map(|file| Relink { place: file.place.clone(), path: path.to_owned() }).collect()
    }
}

/// A folder of the library that is not where the library has it.
#[derive(Clone, Debug, PartialEq)]
pub struct MissingFolder {
    pub path: PathBuf,
    /// How many files the library knew in it.
    pub files: usize,
    /// Where it seems to be now.
    pub found: Option<FoundFolder>,
}

/// A folder that may be a library folder that is missing.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundFolder {
    pub path: PathBuf,
    /// How many of the files the library knew in the missing folder were
    /// looked for in this one, and how many are there with the size they
    /// had.
    pub checked: usize,
    pub there: usize,
}

impl FoundFolder {
    /// Whether it is the folder: most of what was looked for is in it. A
    /// folder that merely has the name is not.
    pub fn sure(&self) -> bool {
        self.there > 0 && self.there * 5 >= self.checked * 4
    }
}

/// What a song is missing, and what was found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Missing {
    /// The files that are not there, in the order the song names them.
    pub files: Vec<MissingFile>,
    /// The folders that went elsewhere, as far as what was found says,
    /// each with how many of the missing files are found by it.
    pub moves: Vec<(FolderMove, usize)>,
    /// The library's own folders that are missing.
    pub folders: Vec<MissingFolder>,
}

impl Missing {
    /// Nothing is missing: not a file of the song, not a folder of the
    /// library.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.folders.is_empty()
    }

    /// How many places of the song name a file that is not there.
    pub fn places(&self) -> usize {
        self.files.iter().map(|file| file.uses.len()).sum()
    }

    /// Whether `place` is one that names a file that is not there.
    pub fn names(&self, place: &FilePlace) -> bool {
        self.files.iter().flat_map(|file| &file.uses).any(|used| used.place == *place)
    }

    /// The session's problems, as it words them, without the lines that
    /// are each about one place whose file is not there: a front end says
    /// those once, as what is missing, and not again for every clip and
    /// pad. (`clip 7: ...`, and `device 9: pad 36: ...` for a sample.)
    pub fn other_problems(&self, problems: Vec<String>) -> Vec<String> {
        if self.files.is_empty() {
            return problems;
        }
        let said: Vec<String> = self
            .files
            .iter()
            .flat_map(|file| &file.uses)
            .map(|used| match &used.place {
                FilePlace::Clip(clip) => format!("{clip}: "),
                FilePlace::Sample { device, slot } => format!("{device}: {slot}: "),
            })
            .collect();
        problems.into_iter().filter(|problem| !said.iter().any(|said| problem.starts_with(said))).collect()
    }

    /// What is missing on the tracks `only` of `project`, and nothing
    /// else: for whoever hears only those.
    pub fn on_tracks(&self, only: &[TrackId], project: &Project) -> Missing {
        let name = |id: TrackId| project.track(id).map(|track| track.name.clone()).unwrap_or_default();
        let mut files: Vec<MissingFile> = Vec::new();
        for file in &self.files {
            let uses: Vec<FileUse> = file
                .uses
                .iter()
                .filter(|used| used.track.is_some_and(|track| only.contains(&track)))
                .cloned()
                .collect();
            if uses.is_empty() {
                continue;
            }
            let mut tracks: Vec<String> = Vec::new();
            for used in &uses {
                let track = used.track.map(name).unwrap_or_default();
                if !tracks.contains(&track) {
                    tracks.push(track);
                }
            }
            files.push(MissingFile { uses, tracks, ..file.clone() });
        }
        Missing { files, moves: self.moves.clone(), folders: Vec::new() }
    }

    /// The tracks that have such a place, in the song's order.
    pub fn tracks(&self) -> Vec<String> {
        let mut tracks: Vec<String> = Vec::new();
        for name in self.files.iter().flat_map(|file| &file.tracks) {
            if !tracks.contains(name) {
                tracks.push(name.clone());
            }
        }
        tracks
    }

    /// How many of the files were found, at least as surely as `sure`.
    pub fn found(&self, sure: Confidence) -> usize {
        self.files.iter().filter(|file| file.found_at_least(sure).is_some()).count()
    }

    /// What relinks every file that was found at least as surely as
    /// `sure` to what was found for it, in every place that names it.
    pub fn relinks(&self, sure: Confidence) -> Vec<Relink> {
        let found = self.files.iter().filter_map(|file| Some((file, file.found_at_least(sure)?)));
        found.flat_map(|(file, candidate)| file.relinks_to(&candidate.path)).collect()
    }

    /// The folder the files found at least as surely as `sure` are in:
    /// where their folder went if one move finds them all, and else the
    /// folder they have in common.
    pub fn found_in(&self, sure: Confidence) -> Option<String> {
        let paths: Vec<&str> =
            self.files.iter().filter_map(|file| Some(file.found_at_least(sure)?.path.as_str())).collect();
        let first = *paths.first()?;
        let by_one_move =
            self.moves.iter().find(|(moved, _)| paths.iter().all(|path| files::below(&moved.to, path).is_some()));
        if let Some((moved, _)) = by_one_move {
            return Some(moved.to.clone());
        }
        let mut folder = files::folder_of(first);
        while !folder.is_empty() && !paths.iter().all(|path| files::below(folder, path).is_some()) {
            folder = files::folder_of(folder);
        }
        (!folder.is_empty()).then(|| folder.to_owned())
    }

    /// The library folders that were found again for sure, as the moves
    /// that put the library right.
    pub fn library_moves(&self) -> Vec<(PathBuf, PathBuf)> {
        let found = self.folders.iter().filter_map(|folder| Some((folder, folder.found.as_ref()?)));
        found
            .filter(|(_, found)| found.sure())
            .map(|(folder, found)| (folder.path.clone(), found.path.clone()))
            .collect()
    }
}

/// Where else to look, said by whoever asks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    /// Folders to look through for files of the missing names, however
    /// deep: a folder someone pointed at.
    pub folders: Vec<PathBuf>,
    /// Folders that are known to have gone elsewhere: what was under
    /// `from` is looked for under `to`, and taken on that word.
    pub moves: Vec<FolderMove>,
}

/// Who says a folder went elsewhere. The later, the more it counts.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Word {
    /// Nobody: it is read from a file that was found, or from a folder of
    /// the same name nearby.
    Read,
    /// The library: a folder of its own turned up with its files in it.
    Library,
    /// The caller.
    Said,
}

/// How a candidate was come by.
#[derive(Copy, Clone)]
enum How {
    /// A folder that went elsewhere puts it there: with how many of the
    /// missing files, and on whose word.
    Moved { with: usize, word: Word },
    /// It has the name: among how many, and whether in a folder the
    /// caller named.
    Named { among: usize, said: bool },
    /// It is where the song says its file was from the song's folder.
    Beside,
}

/// What is on disk, asked once per path.
#[derive(Default)]
struct Disk {
    files: HashMap<String, bool>,
}

impl Disk {
    fn is_file(&mut self, path: &str) -> bool {
        *self.files.entry(path.to_owned()).or_insert_with(|| Path::new(path).is_file())
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn lower_name(path: &str) -> String {
    files::file_name(path).to_lowercase()
}

fn passed_over(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with('$')
        || PASSED_OVER.iter().any(|skipped| skipped.eq_ignore_ascii_case(name))
}

/// The folders in `folder`, with their names. A folder that cannot be
/// read has none.
fn subfolders(folder: &Path) -> Vec<(PathBuf, String)> {
    let Ok(listing) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut found: Vec<(PathBuf, String)> = listing
        .filter_map(Result::ok)
        .filter(|item| item.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|item| (item.path(), item.file_name().to_string_lossy().into_owned()))
        .filter(|(_, name)| !passed_over(name))
        .collect();
    found.sort();
    found
}

/// Folders named like `gone`, a folder that is not there, near where it
/// was: below the nearest folder above it that still exists, the nearest
/// first, and then below the one or two above that. `budget` is how many
/// folders may still be listed.
fn folders_like(gone: &Path, budget: &mut usize) -> Vec<PathBuf> {
    let Some(name) = gone.file_name().map(|name| name.to_string_lossy().into_owned()) else { return Vec::new() };
    let Some(mut near) = gone.ancestors().skip(1).find(|above| above.is_dir()).map(Path::to_path_buf) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = Vec::new();
    let mut looked: Option<PathBuf> = None;
    for step in 0..3 {
        // Further up only while that is still somebody's folder: not from
        // the top of a drive, nor from the folder all users share.
        if step > 0 && near.components().count() < 4 {
            break;
        }
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(near.clone(), 0)]);
        while let Some((folder, depth)) = queue.pop_front() {
            if *budget == 0 {
                return found;
            }
            *budget -= 1;
            for (path, known) in subfolders(&folder) {
                if looked.as_deref() == Some(path.as_path()) {
                    continue;
                }
                if files::same_path(&known, &name) && path != gone {
                    found.push(path.clone());
                }
                if depth + 1 < NEARBY_DEPTH {
                    queue.push_back((path, depth + 1));
                }
            }
        }
        looked = Some(near.clone());
        let Some(above) = near.parent().map(Path::to_path_buf) else { break };
        near = above;
    }
    found
}

/// Files below `folder` whose names (in lower case) are among `wanted`,
/// added to `found` under their names.
fn files_named(
    folder: &Path,
    wanted: &HashSet<String>,
    budget: &mut usize,
    found: &mut HashMap<String, Vec<(String, bool)>>,
) {
    let mut stack = vec![folder.to_path_buf()];
    while let Some(folder) = stack.pop() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(listing) = std::fs::read_dir(&folder) else { continue };
        for item in listing.filter_map(Result::ok) {
            let name = item.file_name().to_string_lossy().into_owned();
            match item.file_type() {
                Ok(kind) if kind.is_dir() => {
                    if !passed_over(&name) {
                        stack.push(item.path());
                    }
                }
                Ok(_) => {
                    let lower = name.to_lowercase();
                    if wanted.contains(&lower) {
                        let path = text(&item.path());
                        let known = found.entry(lower).or_default();
                        if !known.iter().any(|(held, _)| files::same_path(held, &path)) {
                            known.push((path, true));
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
}

/// How much of what the library knew in its folder `root`, which is
/// missing, is in `there`.
fn check_folder(index: &Index, root: &Path, there: &Path) -> FoundFolder {
    let known = index
        .find_folder(root)
        .and_then(|id| index.folder(id))
        .map_or(&[][..], |folder| &index.entries()[folder.all_files()]);
    let step = known.len().div_ceil(FILES_CHECKED).max(1);
    let (mut checked, mut found) = (0, 0);
    for entry in known.iter().step_by(step) {
        let Ok(rest) = entry.path.strip_prefix(root) else { continue };
        checked += 1;
        let same = std::fs::metadata(there.join(rest)).is_ok_and(|file| file.is_file() && file.len() == entry.size);
        found += usize::from(same);
    }
    FoundFolder { path: there.to_path_buf(), checked, there: found }
}

/// The topmost folder of `path` that is not there: the one that was moved
/// or renamed, as far as the disk says. `None` when the file's own folder
/// exists, and when nothing above it does.
fn folder_that_went(path: &str) -> Option<PathBuf> {
    let mut gone = None;
    for above in Path::new(path).ancestors().skip(1) {
        if above.as_os_str().is_empty() {
            return None;
        }
        if above.is_dir() {
            return gone;
        }
        gone = Some(above.to_path_buf());
    }
    None
}

fn seconds(length: f64) -> String {
    if length < 10.0 { format!("{length:.2} s") } else { format!("{length:.1} s") }
}

/// In what `was` and `is` differ, in words: "2.00 s where that was 4.00 s".
fn difference(was: &KnownFile, is: &KnownFile) -> String {
    match (was.seconds(), is.seconds()) {
        (Some(was), Some(is)) if (was - is).abs() > 0.005 => {
            format!("{} long where that was {}", seconds(is), seconds(was))
        }
        _ if was.sample_rate.is_some() && is.sample_rate.is_some() && was.sample_rate != is.sample_rate => {
            format!("{} Hz where that was {} Hz", is.sample_rate.unwrap_or(0), was.sample_rate.unwrap_or(0))
        }
        _ => match (was.bytes, is.bytes) {
            (Some(was), Some(is)) if was != is => format!("{is} bytes where that was {was}"),
            _ => "its header says something else".to_owned(),
        },
    }
}

/// The files the song open in `session` names that are not there:
/// [`Runtime::files_missing`], for a caller that holds the session.
pub fn files_missing_in(session: &Session) -> Missing {
    let tracks: BTreeMap<TrackId, String> =
        session.project().tracks.iter().map(|track| (track.id, track.name.clone())).collect();
    Missing { files: missing_of(session.files(), &tracks, session.known_files()), ..Missing::default() }
}

impl Runtime {
    /// The files the open song names that are not there, with every place
    /// that names each. Nothing is looked for: this is what says that a
    /// song is not whole, and it costs one look at each file.
    pub fn files_missing(&self) -> Missing {
        let (uses, tracks, table) = {
            let session = self.session.lock();
            let tracks: BTreeMap<TrackId, String> =
                session.project().tracks.iter().map(|track| (track.id, track.name.clone())).collect();
            (session.files(), tracks, session.known_files().clone())
        };
        Missing { files: missing_of(uses, &tracks, &table), ..Missing::default() }
    }

    /// What the open song is missing, and what could be found: see the
    /// module. It reads folders and file headers, which takes from a
    /// moment to (with folders named in `look` that are large) a few
    /// seconds, and it holds the session only to read what the song
    /// names: call it from a thread that may wait.
    pub fn missing_files(&self, look: &Look) -> Missing {
        let (uses, tracks, table, song) = {
            let session = self.session.lock();
            let tracks: BTreeMap<TrackId, String> =
                session.project().tracks.iter().map(|track| (track.id, track.name.clone())).collect();
            (session.files(), tracks, session.known_files().clone(), session.path().map(Path::to_path_buf))
        };
        let mut missing = Missing { files: missing_of(uses, &tracks, &table), ..Missing::default() };
        let index = self.library.index();
        let mut disk = Disk::default();
        let mut nearby = NEARBY_FOLDERS;

        // The library's own folders: one that is missing is looked for
        // near where it was, and known by the files the library knew in it.
        let mut moves: Vec<(FolderMove, Word)> = look.moves.iter().map(|moved| (moved.clone(), Word::Said)).collect();
        for root in self.library.missing_roots() {
            let found = folders_like(&root.path, &mut nearby)
                .into_iter()
                .map(|there| check_folder(&index, &root.path, &there))
                // The one with most of the files; among equals, the nearest.
                .rev()
                .max_by_key(|found| (found.sure(), found.there));
            if let Some(found) = found.as_ref().filter(|found| found.sure()) {
                moves.push((FolderMove::new(text(&root.path), text(&found.path)), Word::Library));
            }
            missing.folders.push(MissingFolder { path: root.path, files: root.files, found });
        }
        if missing.files.is_empty() {
            return missing;
        }

        // What the library had read of a file the song wrote nothing down
        // about: its index keeps what it knew of a folder that is gone.
        for file in &mut missing.files {
            if file.known.is_none() {
                let entry = index.find(Path::new(&file.path)).and_then(|id| index.entry(id));
                file.known = entry.map(|entry| KnownFile {
                    path: file.path.clone(),
                    beside: None,
                    bytes: Some(entry.size),
                    frames: entry.audio.and_then(|audio| audio.frames),
                    sample_rate: entry.audio.map(|audio| audio.sample_rate),
                    channels: entry.audio.map(|audio| audio.channels),
                });
            }
        }

        // Every file of one of the missing names: in the library, and in
        // the folders the caller named.
        let wanted: HashSet<String> = missing.files.iter().map(|file| lower_name(&file.path)).collect();
        let mut named: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        for entry in index.entries() {
            let Some(name) = entry.path.file_name() else { continue };
            let lower = name.to_string_lossy().to_lowercase();
            if wanted.contains(&lower) {
                named.entry(lower).or_default().push((text(&entry.path), false));
            }
        }
        let mut budget = NAMED_FOLDERS;
        for folder in &look.folders {
            files_named(folder, &wanted, &mut budget, &mut named);
        }
        // Only what is there, and is another place than the one that is
        // not: the library still lists the files of a folder that is gone.
        for found in named.values_mut() {
            found.retain(|(path, _)| disk.is_file(path));
        }
        let named_for = |file: &MissingFile| -> Vec<(String, bool)> {
            let found = named.get(&lower_name(&file.path)).map(Vec::as_slice).unwrap_or_default();
            found.iter().filter(|(path, _)| files::alike(&file.path, path) > 0).cloned().collect()
        };

        // The folders that may have gone elsewhere: read from each file
        // that was found by its name, and from folders of the same name
        // near where a missing one was.
        let propose = |moved: FolderMove, moves: &mut Vec<(FolderMove, Word)>| {
            let known = moves
                .iter()
                .any(|(held, _)| files::same_path(&held.from, &moved.from) && files::same_path(&held.to, &moved.to));
            if !known {
                moves.push((moved, Word::Read));
            }
        };
        for file in &missing.files {
            for (candidate, _) in named_for(file) {
                if let Some((moved, _)) = FolderMove::between(&file.path, &candidate) {
                    propose(moved, &mut moves);
                }
            }
        }
        let gone: BTreeSet<PathBuf> = missing.files.iter().filter_map(|file| folder_that_went(&file.path)).collect();
        for folder in gone {
            for there in folders_like(&folder, &mut nearby) {
                propose(FolderMove::new(text(&folder), text(&there)), &mut moves);
            }
        }
        // A folder that was renamed as well as moved shows as its folders
        // each having gone to one of the same name: the move one up from
        // them is the folder itself.
        for at in 0..moves.len() {
            if let Some(above) = moves[at].0.parent() {
                propose(above, &mut moves);
            }
        }
        // Each with the missing files it finds. One that finds none says
        // nothing; and one that is read from a single file found by its
        // name, in a folder of another name, says no more than the name.
        let mut counted: Vec<(FolderMove, Word, Vec<usize>)> = Vec::new();
        for (moved, word) in moves {
            let finds = missing.files.iter().enumerate().filter_map(|(at, file)| {
                moved.apply(&file.path).is_some_and(|there| disk.is_file(&there)).then_some(at)
            });
            let finds: Vec<usize> = finds.collect();
            let same_name = files::same_path(files::file_name(&moved.from), files::file_name(&moved.to));
            if finds.len() >= 2 || (finds.len() == 1 && (word != Word::Read || same_name)) {
                counted.push((moved, word, finds));
            }
        }
        // Whoever said it first, then the one that finds most, then the
        // one that says most exactly which folder it was. One that finds
        // nothing the ones before it do not is left out.
        counted.sort_by(|a, b| (b.1, b.2.len(), b.0.from.len()).cmp(&(a.1, a.2.len(), a.0.from.len())));
        let mut found_so_far: HashSet<usize> = HashSet::new();
        counted.retain(|(_, word, finds)| {
            let news = finds.iter().any(|at| !found_so_far.contains(at));
            found_so_far.extend(finds);
            news || *word != Word::Read
        });
        let counted: Vec<(FolderMove, Word, usize)> =
            counted.into_iter().map(|(moved, word, finds)| (moved, word, finds.len())).collect();

        let song_folder =
            song.as_deref().and_then(|song| std::path::absolute(song).ok()).and_then(|song| song.parent().map(text));
        for file in &mut missing.files {
            let mut found: Vec<(String, How)> = Vec::new();
            let add = |path: String, how: How, found: &mut Vec<(String, How)>| {
                if !found.iter().any(|(held, _)| files::same_path(held, &path)) {
                    found.push((path, how));
                }
            };
            for (moved, word, with) in &counted {
                if let Some(there) = moved.apply(&file.path).filter(|there| disk.is_file(there)) {
                    add(there, How::Moved { with: *with, word: *word }, &mut found);
                }
            }
            let by_name = named_for(file);
            for (path, said) in &by_name {
                add(path.clone(), How::Named { among: by_name.len(), said: *said }, &mut found);
            }
            let beside = file.known.as_ref().and_then(|known| known.beside.as_deref());
            if let Some(there) =
                beside.zip(song_folder.as_deref()).and_then(|(beside, folder)| files::resolve(folder, beside))
                && disk.is_file(&there)
            {
                add(there, How::Beside, &mut found);
            }
            file.candidates = judge(file, found);
        }
        missing.moves = counted.into_iter().map(|(moved, _, count)| (moved, count)).collect();
        missing
    }

    /// Gives places of the open song other files to play: one undo step
    /// ([`trill_engine::Session::relink`]). The files are decoded here
    /// first, with the session free, so that the song keeps playing while
    /// a long one is read. Returns how many places now play another file.
    pub fn relink(&self, relinks: &[Relink]) -> Result<usize, SessionError> {
        let paths: BTreeSet<&str> = relinks.iter().map(|relink| relink.path.as_str()).collect();
        for path in paths {
            // What cannot be read is said by the session, which refuses it.
            let _ = self.media.audio(Path::new(path));
        }
        self.session.lock().relink(relinks)
    }

    /// Tells the library that its folder `from` is at `to` now, and saves
    /// that: see [`trill_library::Library::move_root`]. Favorites and
    /// recently used files that were in it go along.
    pub fn move_library_folder(&self, from: &Path, to: &Path) -> Result<MovedRoot, String> {
        let moved = self.library.move_root(from, to)?;
        if let Err(error) = self.save_library_changes() {
            log::warn!("the library's folders could not be saved: {error}");
        }
        Ok(moved)
    }
}

/// The files among `uses` that are not there, each with all its uses.
fn missing_of(uses: Vec<FileUse>, tracks: &BTreeMap<TrackId, String>, table: &FileTable) -> Vec<MissingFile> {
    let mut files: Vec<MissingFile> = Vec::new();
    let mut there: HashMap<String, bool> = HashMap::new();
    for file in uses {
        if *there.entry(file.path.clone()).or_insert_with(|| Path::new(&file.path).is_file()) {
            continue;
        }
        let track = file.track.and_then(|id| tracks.get(&id)).cloned().unwrap_or_else(|| "Master".to_owned());
        let at = files.iter().position(|known| known.path == file.path).unwrap_or_else(|| {
            let known = table.get(&file.path).filter(|known| known.says_what_it_was()).cloned();
            files.push(MissingFile {
                path: file.path.clone(),
                uses: Vec::new(),
                tracks: Vec::new(),
                known,
                candidates: Vec::new(),
            });
            files.len() - 1
        });
        if !files[at].tracks.contains(&track) {
            files[at].tracks.push(track);
        }
        files[at].uses.push(file);
    }
    files
}

/// Says of each thing found for `file` how sure it is, and puts the best
/// first.
fn judge(file: &MissingFile, found: Vec<(String, How)>) -> Vec<Candidate> {
    // How far into the file its clips play, and which plays furthest.
    let reach = file.uses.iter().filter_map(|used| Some((used.reach_seconds?, used.name.as_str())));
    let reach = reach.max_by(|a, b| a.0.total_cmp(&b.0));
    let facts: Vec<Option<KnownFile>> = found.iter().map(|(path, _)| file_facts(Path::new(path), None)).collect();
    let same: Vec<Option<bool>> = facts
        .iter()
        .map(|facts| file.known.as_ref().zip(facts.as_ref()).and_then(|(known, facts)| known.is(facts)))
        .collect();
    let the_same = same.iter().filter(|same| **same == Some(true)).count();

    let mut judged: Vec<(Candidate, usize)> = Vec::new();
    for (((path, how), facts), same) in found.into_iter().zip(&facts).zip(&same) {
        let (base, mut why) = match how {
            How::Moved { word: Word::Said, .. } => {
                (Confidence::Sure, "where the folder that was named puts it".to_owned())
            }
            How::Moved { word: Word::Library, .. } => (Confidence::Sure, "where the library's folder went".to_owned()),
            How::Moved { with, .. } if with >= 2 => (
                Confidence::Sure,
                match with - 1 {
                    1 => "where its folder went, like 1 other file of the song".to_owned(),
                    others => format!("where its folder went, like {others} other files of the song"),
                },
            ),
            How::Moved { .. } => (Confidence::Likely, "at the same place in a folder of the same name".to_owned()),
            How::Named { among: 1, said: true } => {
                (Confidence::Sure, "the only file of that name in the folder that was named".to_owned())
            }
            How::Named { among: 1, .. } => (Confidence::Likely, "the only file of that name in the library".to_owned()),
            How::Named { among, .. } => (Confidence::Unsure, format!("one of {among} files of that name")),
            How::Beside => {
                (Confidence::Likely, "beside the song, where the file was when the song was saved".to_owned())
            }
        };
        let mut confidence = match same {
            Some(false) => {
                let differs = file.known.as_ref().zip(facts.as_ref()).map(|(known, facts)| difference(known, facts));
                why.push_str(&format!(", but it is not the file the song knew: {}", differs.unwrap_or_default()));
                Confidence::Unsure
            }
            Some(true) => {
                why.push_str(", and it has the size and length of the file the song knew");
                // Among files of one name, only the one that is alone in
                // being what the song knew is the file for sure.
                if base >= Confidence::Likely || the_same == 1 { Confidence::Sure } else { Confidence::Likely }
            }
            None => base,
        };
        let length = facts.as_ref().and_then(KnownFile::seconds);
        if let (Some((reach, clip)), Some(length)) = (reach, length)
            && *same != Some(false)
            && reach > length + REACH_SLACK
        {
            why.push_str(&format!(
                ", but it is {} long and the clip \u{201C}{clip}\u{201D} plays {} of its file",
                seconds(length),
                seconds(reach)
            ));
            confidence = match confidence {
                Confidence::Sure => Confidence::Likely,
                _ => Confidence::Unsure,
            };
        }
        let alike = files::alike(&file.path, &path);
        judged.push((Candidate { path, confidence, why, seconds: length }, alike));
    }
    // The surest first; among equals the one whose folders are most like
    // the missing file's. The order they were found in settles the rest.
    judged.sort_by(|a, b| (b.0.confidence, b.1).cmp(&(a.0.confidence, a.1)));
    // Two that are equally the best cannot both be the file for sure.
    if let [(first, first_alike), (second, second_alike), ..] = judged.as_mut_slice()
        && first.confidence == Confidence::Sure
        && second.confidence == Confidence::Sure
        && first_alike == second_alike
    {
        first.confidence = Confidence::Likely;
        first.why.push_str(", and so is another file");
        second.confidence = Confidence::Likely;
    }
    judged.into_iter().map(|(candidate, _)| candidate).take(CANDIDATES).collect()
}
