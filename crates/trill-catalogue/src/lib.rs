//! The catalogue: sampled instruments that others publish and that Trill
//! may hand on, each with what it is, its licence in its maker's own
//! words, whom to name for it, where it is fetched from and what it takes
//! to know that what arrived is what was read.
//!
//! Trill ships **no recording**. The catalogue is text (`catalogue/` at
//! the root of the repository, built into the program), and a user fetches
//! what they choose from where its maker keeps it. What is in it, and what
//! was left out and why, is decided by the rules in `catalogue/README.md`:
//! only sets whose licence lets anyone hand them on (dedicated to the
//! public domain, or asking for no more than to be named), each licence
//! read at its source and quoted.
//!
//! * A [`Set`] is what is fetched as one thing: a maker's set, at one
//!   version of it. It says how ([`Fetch`]): as the archive its source
//!   makes of that version, pinned by what it unpacks to
//!   ([`check::tree_sum`]), or file by file, each pinned by its own
//!   checksum. It has room for other places to fetch it from
//!   ([`Mirror`]).
//! * An [`Instrument`] is one file of a set that the Sampled Instrument
//!   plays: its kind, a sentence a search finds it by, its range, layers,
//!   takes and size, what was measured of its recordings, and what its
//!   file asks for that the player does not do.
//! * [`Catalogue::owed`] says what a song owes: from the instruments it
//!   plays, the lines of the makers who ask to be named.
//!
//! Nothing here fetches anything, and nothing reads a file but
//! [`check`], which is handed a folder.

pub mod check;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The catalogue as it is built into the program.
const INSTRUMENTS: &str = include_str!("../../../catalogue/instruments.toml");
/// The files of the sets that are fetched file by file.
const FILES: &str = include_str!("../../../catalogue/files.toml");
/// What was found and is not offered, with the reason.
const LEFT_OUT: &str = include_str!("../../../catalogue/left-out.toml");

/// The format of the catalogue's files this program reads.
pub const FORMAT: u32 = 1;

/// A licence that lets anyone hand a set on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Licence {
    /// Creative Commons Zero 1.0: dedicated to the public domain.
    Cc0,
    /// The Unlicense: released into the public domain.
    Unlicense,
    /// Creative Commons Attribution 3.0 Unported.
    CcBy3,
    /// Creative Commons Attribution 4.0 International.
    CcBy4,
}

impl Licence {
    pub const ALL: [Licence; 4] = [Licence::Cc0, Licence::Unlicense, Licence::CcBy3, Licence::CcBy4];

    /// As the catalogue writes it, and as licences are told apart by
    /// programs: `CC0-1.0`, `CC-BY-4.0`.
    pub fn id(self) -> &'static str {
        match self {
            Licence::Cc0 => "CC0-1.0",
            Licence::Unlicense => "Unlicense",
            Licence::CcBy3 => "CC-BY-3.0",
            Licence::CcBy4 => "CC-BY-4.0",
        }
    }

    /// As it is said to a person.
    pub fn name(self) -> &'static str {
        match self {
            Licence::Cc0 => "CC0 1.0 (public domain)",
            Licence::Unlicense => "The Unlicense (public domain)",
            Licence::CcBy3 => "CC BY 3.0 (attribution)",
            Licence::CcBy4 => "CC BY 4.0 (attribution)",
        }
    }

    /// Whether whoever hands the set on, or releases music made with it,
    /// is asked to name its maker.
    pub fn asks_credit(self) -> bool {
        matches!(self, Licence::CcBy3 | Licence::CcBy4)
    }

    pub fn of(id: &str) -> Option<Licence> {
        Licence::ALL.into_iter().find(|licence| licence.id().eq_ignore_ascii_case(id))
    }
}

/// What kind of instrument one is: what a list of them is grouped by.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Piano,
    ElectricPiano,
    Organ,
    /// Harpsichords, clavichords, celestas, accordions: keys that are
    /// none of the above.
    Keyboard,
    Guitar,
    Bass,
    /// Bowed strings, alone and in sections.
    Strings,
    Brass,
    Woodwind,
    Voice,
    /// Struck bars, bells and tines that play pitches.
    Mallets,
    /// Harps, zithers, banjos and whatever else is plucked.
    Plucked,
    DrumKit,
    /// Percussion that is not a kit: hand drums, orchestral percussion,
    /// single instruments.
    Percussion,
    Synth,
    /// Sounds that are no instrument: noises, textures, effects.
    Effects,
}

impl Kind {
    pub const ALL: [Kind; 16] = [
        Kind::Piano,
        Kind::ElectricPiano,
        Kind::Organ,
        Kind::Keyboard,
        Kind::Guitar,
        Kind::Bass,
        Kind::Strings,
        Kind::Brass,
        Kind::Woodwind,
        Kind::Voice,
        Kind::Mallets,
        Kind::Plucked,
        Kind::DrumKit,
        Kind::Percussion,
        Kind::Synth,
        Kind::Effects,
    ];

    /// As the catalogue writes it and a search asks for it.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Piano => "piano",
            Kind::ElectricPiano => "electric-piano",
            Kind::Organ => "organ",
            Kind::Keyboard => "keyboard",
            Kind::Guitar => "guitar",
            Kind::Bass => "bass",
            Kind::Strings => "strings",
            Kind::Brass => "brass",
            Kind::Woodwind => "woodwind",
            Kind::Voice => "voice",
            Kind::Mallets => "mallets",
            Kind::Plucked => "plucked",
            Kind::DrumKit => "drum-kit",
            Kind::Percussion => "percussion",
            Kind::Synth => "synth",
            Kind::Effects => "effects",
        }
    }

    /// As a heading says it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Piano => "Pianos",
            Kind::ElectricPiano => "Electric pianos",
            Kind::Organ => "Organs",
            Kind::Keyboard => "Other keyboards",
            Kind::Guitar => "Guitars",
            Kind::Bass => "Basses",
            Kind::Strings => "Strings",
            Kind::Brass => "Brass",
            Kind::Woodwind => "Woodwinds",
            Kind::Voice => "Voices",
            Kind::Mallets => "Mallets and bells",
            Kind::Plucked => "Plucked strings",
            Kind::DrumKit => "Drum kits",
            Kind::Percussion => "Percussion",
            Kind::Synth => "Synthesizers",
            Kind::Effects => "Noises and effects",
        }
    }

    pub fn of(id: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.id().eq_ignore_ascii_case(id))
    }
}

/// What was found of an instrument that whoever chooses one should know.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mark {
    /// The best of its kind that was found: among what is fetched first.
    Starter,
    /// Its recordings carry noise that is heard.
    Noisy,
    /// It is not in tune with itself, or as a whole.
    OutOfTune,
    /// Its level jumps from one recorded key to the next, at most of them.
    Uneven,
    /// It came out of the player silent, or all but: its `note` says
    /// when it is only waiting for a controller to be moved.
    Silent,
    /// Its file names recordings its set does not have.
    Incomplete,
    /// Its file asks for something the player does not do and that is
    /// most of its sound: what is heard is not what its maker meant.
    Unfaithful,
}

impl Mark {
    pub const ALL: [Mark; 7] =
        [Mark::Starter, Mark::Noisy, Mark::OutOfTune, Mark::Uneven, Mark::Silent, Mark::Incomplete, Mark::Unfaithful];

    pub fn id(self) -> &'static str {
        match self {
            Mark::Starter => "starter",
            Mark::Noisy => "noisy",
            Mark::OutOfTune => "out-of-tune",
            Mark::Uneven => "uneven",
            Mark::Silent => "silent",
            Mark::Incomplete => "incomplete",
            Mark::Unfaithful => "unfaithful",
        }
    }

    /// What it says, to someone choosing.
    pub fn words(self) -> &'static str {
        match self {
            Mark::Starter => "among the best of its kind that were found",
            Mark::Noisy => "its recordings carry noise that is heard",
            Mark::OutOfTune => "it is out of tune",
            Mark::Uneven => "its level jumps from key to key",
            Mark::Silent => "it is silent as it loads",
            Mark::Incomplete => "its file names recordings that its set does not have",
            Mark::Unfaithful => "the player does not do what most of its sound is made by",
        }
    }

    /// Whether it speaks against the instrument.
    pub fn is_fault(self) -> bool {
        self != Mark::Starter
    }

    pub fn of(id: &str) -> Option<Mark> {
        Mark::ALL.into_iter().find(|mark| mark.id().eq_ignore_ascii_case(id))
    }
}

/// One file of a set that is fetched file by file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOf {
    /// Below the set's folder, with forward slashes.
    pub path: String,
    pub size: u64,
    /// Its SHA-256, in hexadecimal.
    pub sha256: String,
}

/// How a set is fetched from where its maker keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetch {
    /// As one archive (a zip), of which everything below `under` is the
    /// set. The archive is made by its source each time it is asked for,
    /// so it is pinned by what it unpacks to: `sum` is the
    /// [`check::tree_sum`] of the set's files. `bytes` is what the archive
    /// took the day it was fetched and checked, which is an estimate of
    /// what it takes the next time (0 where it was never fetched so).
    Archive { url: String, under: String, sum: String, bytes: u64 },
    /// File by file: each of `files` is at `base` followed by its path
    /// (with the characters an address cannot hold written as `%XX`),
    /// and is pinned by its own checksum.
    Files { base: String, files: Vec<FileOf> },
}

/// Another place a set can be fetched from: an archive that somebody
/// keeps, whose own bytes are pinned. None is listed today; the format
/// has the room.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Mirror {
    pub url: String,
    /// The archive's SHA-256, in hexadecimal.
    pub sha256: String,
    pub bytes: u64,
    /// What in the archive is the set: everything below this.
    #[serde(default)]
    pub under: String,
}

/// One instrument of a set.
#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    /// Its file, below the set's folder, with forward slashes.
    pub file: String,
    /// The SHA-256 of that file, in hexadecimal: what an instrument on a
    /// disk is known again by, wherever it was put.
    pub text: String,
    pub name: String,
    pub kind: Kind,
    /// What it is, in a sentence a search finds it by.
    pub words: String,
    /// The lowest and highest key that sounds.
    pub range: Option<(u8, u8)>,
    /// Its keys are drums, not pitches.
    pub drums: bool,
    /// The most layers of loudness a key has, and the most recordings of
    /// one note at one loudness.
    pub layers: u32,
    pub takes: u32,
    /// How many recordings it plays, and what they take on a disk.
    pub recordings: u32,
    pub bytes: u64,
    /// The ways of playing a key of it chooses, by name.
    pub ways: Vec<String>,
    /// How far under its peak the quietest twentieth of a second of a
    /// recording lies, in dB: at the middle recording, and for the
    /// noisiest tenth. `None` where that says nothing (see `note`).
    pub noise: Option<(f32, f32)>,
    /// How far from equal temperament its recorded keys are, in cents:
    /// the middle one of them (the median), how far a key usually lies
    /// from that (the median of the distances), and the worst. Middles,
    /// so that one key whose pitch was read wrongly decides nothing.
    pub tuning: Option<(f32, f32, f32)>,
    /// How its level steps from one recorded key to the next through the
    /// player, in dB: the usual step (the median), and the most.
    pub evenness: Option<(f32, f32)>,
    /// The peak of a few notes of it out of the player at its own
    /// volume, in dB.
    pub level: Option<f32>,
    pub marks: Vec<Mark>,
    /// What its file asks for that the player does not do, in the
    /// player's own sentence. `None` for one that is played as its maker
    /// meant.
    pub misses: Option<String>,
    /// What else there is to know: why a figure is not given.
    pub note: Option<String>,
}

impl Instrument {
    /// Whether nothing that was measured speaks against it.
    pub fn is_put_forward(&self) -> bool {
        !self.marks.iter().any(|mark| mark.is_fault())
    }

    pub fn is_starter(&self) -> bool {
        self.marks.contains(&Mark::Starter)
    }
}

/// What is fetched as one thing: a maker's set at one version.
#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    /// The name of its folder: lower-case letters, digits and dashes.
    pub id: String,
    pub name: String,
    /// What it is, in a sentence.
    pub about: String,
    /// Who made it.
    pub by: String,
    pub licence: Licence,
    /// Where the licence was read, at the version that is fetched.
    pub licence_at: String,
    /// The sentence there that allows it, as it stands.
    pub licence_says: String,
    /// The line to show for it, where its maker asks to be named.
    pub credit: Option<String>,
    /// Where it is published.
    pub from: String,
    /// The version that was read and is fetched: a commit of its source.
    pub version: String,
    /// The day its licence and its files were read.
    pub read: String,
    /// What all its files take on a disk, and how many there are.
    pub bytes: u64,
    pub files: u32,
    pub fetch: Fetch,
    pub mirrors: Vec<Mirror>,
    pub instruments: Vec<Instrument>,
}

impl Set {
    /// Whether it is among what is fetched first: one of its instruments
    /// is the best of its kind.
    pub fn is_starter(&self) -> bool {
        self.instruments.iter().any(Instrument::is_starter)
    }

    /// The checksum of what it is on a disk, whichever way it came there
    /// ([`check::tree_sum`]): what an archive of it kept anywhere has to
    /// unpack to.
    pub fn tree(&self) -> String {
        match &self.fetch {
            Fetch::Archive { sum, .. } => sum.clone(),
            Fetch::Files { files, .. } => {
                check::tree_sum_of(files.iter().map(|file| (file.path.as_str(), file.sha256.as_str())))
            }
        }
    }

    /// What is fetched for it, in bytes: an estimate for an archive its
    /// source makes when it is asked for.
    pub fn fetched_bytes(&self) -> u64 {
        match &self.fetch {
            Fetch::Archive { bytes, .. } if *bytes > 0 => *bytes,
            _ => self.bytes,
        }
    }

    /// The kinds of its instruments, each once.
    pub fn kinds(&self) -> Vec<Kind> {
        let mut kinds: Vec<Kind> = self.instruments.iter().map(|instrument| instrument.kind).collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    /// The line to show for it: its maker's where one is asked for.
    pub fn credit_line(&self) -> Option<Credit> {
        let line = self.credit.clone()?;
        Some(Credit {
            set: self.id.clone(),
            name: self.name.clone(),
            line,
            licence: self.licence,
            at: self.licence_at.clone(),
        })
    }
}

/// Why something that was found is not offered.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// Its licence asks that whatever is made from it be shared under the
    /// same licence. Listed apart: the owner decides.
    ShareAlike,
    /// It may not be used, or handed on, for money.
    NonCommercial,
    /// Its own terms forbid handing it on, or passing it on in a
    /// collection.
    OwnTerms,
    /// It says no licence, or one that cannot be told.
    Unclear,
    /// It is published in a form Trill does not fetch.
    Form,
}

impl Reason {
    pub const ALL: [Reason; 5] =
        [Reason::ShareAlike, Reason::NonCommercial, Reason::OwnTerms, Reason::Unclear, Reason::Form];

    pub fn id(self) -> &'static str {
        match self {
            Reason::ShareAlike => "share-alike",
            Reason::NonCommercial => "non-commercial",
            Reason::OwnTerms => "own-terms",
            Reason::Unclear => "unclear",
            Reason::Form => "form",
        }
    }

    pub fn of(id: &str) -> Option<Reason> {
        Reason::ALL.into_iter().find(|reason| reason.id().eq_ignore_ascii_case(id))
    }
}

/// Something that was found and is not offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOut {
    pub name: String,
    /// Where it is published.
    pub from: String,
    pub reason: Reason,
    /// Its terms as they were read, quoted, or that none were found.
    pub says: String,
}

/// A maker's line, for a set that asks to be named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credit {
    /// The set's id and its name.
    pub set: String,
    pub name: String,
    /// The line to show.
    pub line: String,
    pub licence: Licence,
    /// Where the licence was read.
    pub at: String,
}

/// What a song owes for the sampled instruments it plays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Owed {
    /// The lines of the makers who ask to be named, each once, with the
    /// instruments of the song they are owed for.
    pub lines: Vec<(Credit, Vec<String>)>,
    /// Instruments of the catalogue whose makers ask for nothing.
    pub free: Vec<String>,
    /// Instrument files the catalogue does not know: whatever they owe
    /// is in their own licence.
    pub unknown: Vec<String>,
}

impl Owed {
    /// The lines as text to paste where a song is released; empty when
    /// nothing is owed.
    pub fn text(&self) -> String {
        self.lines.iter().map(|(credit, _)| credit.line.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// What a search of the catalogue asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Search {
    /// Words, every one of which must be in the instrument's name, its
    /// sentence, its kind, or its set's name or maker. Any case.
    pub words: String,
    pub kind: Option<Kind>,
    /// Only what asks for no credit.
    pub public_domain_only: bool,
    /// Only what is fetched first.
    pub starter_only: bool,
    /// Also what something measured speaks against.
    pub with_faults: bool,
}

/// The whole catalogue.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalogue {
    pub sets: Vec<Set>,
    pub left_out: Vec<LeftOut>,
}

// ---- the files as they are written ----------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenFetch {
    #[serde(default)]
    archive: Option<String>,
    #[serde(default)]
    under: Option<String>,
    #[serde(default)]
    sum: Option<String>,
    /// What the archive took when it was fetched and checked.
    #[serde(default)]
    archive_bytes: Option<u64>,
    #[serde(default)]
    base: Option<String>,
    /// The name of a list in `files.toml`.
    #[serde(default)]
    list: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenInstrument {
    file: String,
    text: String,
    name: String,
    kind: String,
    words: String,
    #[serde(default)]
    range: Option<(u8, u8)>,
    #[serde(default)]
    drums: bool,
    #[serde(default)]
    layers: u32,
    #[serde(default)]
    takes: u32,
    recordings: u32,
    bytes: u64,
    #[serde(default)]
    ways: Vec<String>,
    #[serde(default)]
    noise: Option<(f32, f32)>,
    #[serde(default)]
    tuning: Option<(f32, f32, f32)>,
    #[serde(default)]
    evenness: Option<(f32, f32)>,
    #[serde(default)]
    level: Option<f32>,
    #[serde(default)]
    marks: Vec<String>,
    #[serde(default)]
    misses: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenSet {
    id: String,
    name: String,
    about: String,
    by: String,
    licence: String,
    licence_at: String,
    licence_says: String,
    #[serde(default)]
    credit: Option<String>,
    from: String,
    version: String,
    read: String,
    bytes: u64,
    files: u32,
    fetch: WrittenFetch,
    #[serde(default, rename = "mirror")]
    mirrors: Vec<Mirror>,
    #[serde(default, rename = "instrument")]
    instruments: Vec<WrittenInstrument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenCatalogue {
    format: u32,
    #[serde(default, rename = "set")]
    sets: Vec<WrittenSet>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenFiles {
    format: u32,
    /// By the name a set's `fetch.list` gives: a line for each file,
    /// `<sha256> <size> <path>`.
    #[serde(default)]
    list: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenLeftOut {
    name: String,
    from: String,
    reason: String,
    says: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenLeftOuts {
    format: u32,
    #[serde(default, rename = "left_out")]
    left_out: Vec<WrittenLeftOut>,
}

/// Whether `id` can be the name of a folder on any system, and of a part
/// that is asked for by it: lower-case letters, digits and dashes.
fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Whether a path stays below the folder it is put under, whatever the
/// system: no way up, no drive, no backslash.
fn is_path_below(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn list_of(text: &str, name: &str) -> Result<Vec<FileOf>, String> {
    let mut files = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(3, ' ');
        let (Some(sha256), Some(size), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            return Err(format!("the list '{name}' has a line that is not '<sha256> <size> <path>': {line}"));
        };
        let size: u64 = size.parse().map_err(|_| format!("the list '{name}' gives '{size}' as the size of {path}"))?;
        if !is_sha256(sha256) || !is_path_below(path) {
            return Err(format!("the list '{name}' has a line whose checksum or path cannot be one: {line}"));
        }
        files.push(FileOf { path: path.to_string(), size, sha256: sha256.to_string() });
    }
    if files.is_empty() {
        return Err(format!("the list '{name}' names no file"));
    }
    Ok(files)
}

impl Catalogue {
    /// The catalogue built into the program. If its text cannot be read
    /// (which a test would have caught) it is empty, and
    /// [`Catalogue::problem`] says why.
    pub fn shipped() -> &'static Catalogue {
        static SHIPPED: OnceLock<(Catalogue, Option<String>)> = OnceLock::new();
        &SHIPPED
            .get_or_init(|| match Catalogue::read(INSTRUMENTS, FILES, LEFT_OUT) {
                Ok(catalogue) => (catalogue, None),
                Err(problem) => (Catalogue::default(), Some(problem)),
            })
            .0
    }

    /// Why the built-in catalogue is empty, if it could not be read.
    pub fn problem() -> Option<String> {
        Catalogue::read(INSTRUMENTS, FILES, LEFT_OUT).err()
    }

    /// Reads a catalogue from the text of its three files. An error says
    /// which entry is wrong and how: nothing of a catalogue with a wrong
    /// entry is taken.
    pub fn read(instruments: &str, files: &str, left_out: &str) -> Result<Catalogue, String> {
        let written: WrittenCatalogue =
            toml::from_str(instruments).map_err(|error| format!("the catalogue cannot be read: {error}"))?;
        let lists: WrittenFiles =
            toml::from_str(files).map_err(|error| format!("the catalogue's file lists cannot be read: {error}"))?;
        let outs: WrittenLeftOuts = toml::from_str(left_out)
            .map_err(|error| format!("the list of what was left out cannot be read: {error}"))?;
        for (what, format) in [("catalogue", written.format), ("file lists", lists.format), ("left out", outs.format)] {
            if format != FORMAT {
                return Err(format!("the {what} is of format {format}, and this program reads format {FORMAT}"));
            }
        }
        let mut sets: Vec<Set> = Vec::with_capacity(written.sets.len());
        for set in written.sets {
            let id = set.id.clone();
            let wrong = |what: String| format!("the set '{id}': {what}");
            if !is_id(&set.id) {
                return Err(wrong("its id is not lower-case letters, digits and dashes".into()));
            }
            if sets.iter().any(|known| known.id == set.id) {
                return Err(wrong("there are two of it".into()));
            }
            let licence = Licence::of(&set.licence)
                .ok_or_else(|| wrong(format!("'{}' is not a licence the catalogue takes", set.licence)))?;
            if set.licence_says.trim().is_empty() || !set.licence_at.starts_with("https://") {
                return Err(wrong("it does not say where its licence was read, or what the licence says".into()));
            }
            if licence.asks_credit() && set.credit.as_deref().is_none_or(|line| line.trim().is_empty()) {
                return Err(wrong("its licence asks that its maker be named, and it has no credit line".into()));
            }
            let fetch = match set.fetch {
                WrittenFetch {
                    archive: Some(url),
                    under: Some(under),
                    sum: Some(sum),
                    archive_bytes,
                    base: None,
                    list: None,
                } => {
                    if !url.starts_with("https://") || !is_sha256(&sum) || !(under.is_empty() || under.ends_with('/')) {
                        return Err(wrong("its archive, what it is under or its checksum cannot be one".into()));
                    }
                    Fetch::Archive { url, under, sum, bytes: archive_bytes.unwrap_or(0) }
                }
                WrittenFetch {
                    archive: None,
                    under: None,
                    sum: None,
                    archive_bytes: None,
                    base: Some(base),
                    list: Some(list),
                } => {
                    if !base.starts_with("https://") || !base.ends_with('/') {
                        return Err(wrong("the address its files are below is not one".into()));
                    }
                    let text = lists.list.get(&list).ok_or_else(|| wrong(format!("there is no list '{list}'")))?;
                    Fetch::Files { base, files: list_of(text, &list).map_err(wrong)? }
                }
                _ => {
                    return Err(wrong(
                        "it is fetched as an archive (archive, under, sum) or file by file (base, list), not both"
                            .into(),
                    ));
                }
            };
            let mut instruments = Vec::with_capacity(set.instruments.len());
            for instrument in set.instruments {
                let file = instrument.file.clone();
                let wrong = |what: String| format!("the set '{id}', '{file}': {what}");
                let kind = Kind::of(&instrument.kind)
                    .ok_or_else(|| wrong(format!("'{}' is not a kind of instrument", instrument.kind)))?;
                let mut marks = Vec::new();
                for mark in &instrument.marks {
                    marks.push(Mark::of(mark).ok_or_else(|| wrong(format!("'{mark}' is not a mark")))?);
                }
                if !is_path_below(&instrument.file) || !is_sha256(&instrument.text) {
                    return Err(wrong("its file or its checksum cannot be one".into()));
                }
                if let Fetch::Files { files, .. } = &fetch
                    && !files.iter().any(|listed| listed.path == instrument.file && listed.sha256 == instrument.text)
                {
                    return Err(wrong("it is not among the files its set is fetched as".into()));
                }
                if instrument.words.trim().is_empty() || instrument.name.trim().is_empty() {
                    return Err(wrong("it has no name, or no sentence to be found by".into()));
                }
                instruments.push(Instrument {
                    file: instrument.file,
                    text: instrument.text,
                    name: instrument.name,
                    kind,
                    words: instrument.words,
                    range: instrument.range,
                    drums: instrument.drums,
                    layers: instrument.layers,
                    takes: instrument.takes,
                    recordings: instrument.recordings,
                    bytes: instrument.bytes,
                    ways: instrument.ways,
                    noise: instrument.noise,
                    tuning: instrument.tuning,
                    evenness: instrument.evenness,
                    level: instrument.level,
                    marks,
                    misses: instrument.misses,
                    note: instrument.note,
                });
            }
            if instruments.is_empty() {
                return Err(wrong("it has no instrument".into()));
            }
            sets.push(Set {
                id: set.id,
                name: set.name,
                about: set.about,
                by: set.by,
                licence,
                licence_at: set.licence_at,
                licence_says: set.licence_says,
                credit: set.credit.filter(|line| !line.trim().is_empty()),
                from: set.from,
                version: set.version,
                read: set.read,
                bytes: set.bytes,
                files: set.files,
                fetch,
                mirrors: set.mirrors,
                instruments,
            });
        }
        let mut left_out = Vec::with_capacity(outs.left_out.len());
        for out in outs.left_out {
            let reason = Reason::of(&out.reason)
                .ok_or_else(|| format!("'{}' was left out for '{}', which is no reason", out.name, out.reason))?;
            left_out.push(LeftOut { name: out.name, from: out.from, reason, says: out.says });
        }
        Ok(Catalogue { sets, left_out })
    }

    pub fn set(&self, id: &str) -> Option<&Set> {
        self.sets.iter().find(|set| set.id == id)
    }

    /// Every instrument with its set, in the catalogue's order.
    pub fn instruments(&self) -> impl Iterator<Item = (&Set, &Instrument)> {
        self.sets.iter().flat_map(|set| set.instruments.iter().map(move |instrument| (set, instrument)))
    }

    /// The instrument whose file has this SHA-256: how one on a disk is
    /// known again, wherever it was put and however it came there.
    pub fn by_text(&self, sha256: &str) -> Option<(&Set, &Instrument)> {
        self.instruments().find(|(_, instrument)| instrument.text.eq_ignore_ascii_case(sha256))
    }

    /// The instruments a search asks for: by kind, then the ones that are
    /// fetched first, then by name.
    pub fn find(&self, search: &Search) -> Vec<(&Set, &Instrument)> {
        let words: Vec<String> = search.words.split_whitespace().map(str::to_lowercase).collect();
        let mut found: Vec<(&Set, &Instrument)> = self
            .instruments()
            .filter(|(set, instrument)| {
                search.kind.is_none_or(|kind| instrument.kind == kind)
                    && (!search.public_domain_only || !set.licence.asks_credit())
                    && (!search.starter_only || instrument.is_starter())
                    && (search.with_faults || instrument.is_put_forward())
                    && {
                        let text = format!(
                            "{} {} {} {} {} {}",
                            instrument.name,
                            instrument.words,
                            instrument.kind.id(),
                            set.name,
                            set.by,
                            set.id
                        )
                        .to_lowercase();
                        words.iter().all(|word| text.contains(word.as_str()))
                    }
            })
            .collect();
        found.sort_by(|a, b| {
            (a.1.kind, !a.1.is_starter(), a.1.name.to_lowercase(), &a.0.id).cmp(&(
                b.1.kind,
                !b.1.is_starter(),
                b.1.name.to_lowercase(),
                &b.0.id,
            ))
        });
        found
    }

    /// The sets that are fetched first: the best of each kind.
    pub fn starter(&self) -> Vec<&Set> {
        self.sets.iter().filter(|set| set.is_starter()).collect()
    }

    /// The lines of every set that asks to be named, by the set's name:
    /// what is shown where the program says what it offers.
    pub fn credits(&self) -> Vec<Credit> {
        let mut credits: Vec<Credit> = self.sets.iter().filter_map(Set::credit_line).collect();
        credits.sort_by_key(|credit| credit.name.to_lowercase());
        credits
    }

    /// What is owed for instruments: each given as what it is called in
    /// the song (a track's name, a file's) and the SHA-256 of its file.
    pub fn owed<'a>(&self, played: impl IntoIterator<Item = (&'a str, &'a str)>) -> Owed {
        let mut owed = Owed::default();
        for (called, text) in played {
            match self.by_text(text) {
                Some((set, instrument)) => match set.credit_line() {
                    Some(credit) => {
                        let used = format!("{} ({called})", instrument.name);
                        match owed.lines.iter_mut().find(|(known, _)| known.set == credit.set) {
                            Some((_, uses)) if !uses.contains(&used) => uses.push(used),
                            Some(_) => {}
                            None => owed.lines.push((credit, vec![used])),
                        }
                    }
                    None => {
                        let free = format!("{} ({})", instrument.name, set.name);
                        if !owed.free.contains(&free) {
                            owed.free.push(free);
                        }
                    }
                },
                None => {
                    if !owed.unknown.iter().any(|known| known == called) {
                        owed.unknown.push(called.to_string());
                    }
                }
            }
        }
        owed.lines.sort_by_key(|(credit, _)| credit.name.to_lowercase());
        owed
    }
}

#[cfg(test)]
mod tests;
