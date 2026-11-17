//! Searching the index.
//!
//! A search is meant to run on every keystroke, over a hundred thousand
//! files, without the list ever feeling behind the typing. So it never
//! looks at entries one string at a time: each word of the query is found
//! in one pass over all the file names laid end to end, and one over the
//! folder paths, and the filters test a compact table of facts.

use crate::entry::EntryKind;
use crate::index::{EntryId, Facts, Index, MatchPlace};
use crate::infer::is_word_or_plural;
use crate::key::Key;
use crate::tags::{Tags, VOCABULARY};
use std::ops::RangeInclusive;
use std::path::PathBuf;

/// What to look for. The default query matches every readable file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    /// Words to look for, separated by spaces, in any letter case. Every
    /// word must be found in the file's name, in the names of its folders
    /// from the library root down, or among its tags, tempo (`140bpm`) and
    /// key (`f#m`).
    pub text: String,
    /// Only files of this kind.
    pub kind: Option<EntryKind>,
    /// Only files that carry every one of these tags. Loops and one-shots
    /// are tags too: [`Tag::Loop`](crate::Tag::Loop) and
    /// [`Tag::OneShot`](crate::Tag::OneShot).
    pub tags: Tags,
    /// Only files whose tempo, in beats per minute, is known and in this
    /// range.
    pub tempo: Option<RangeInclusive<f32>>,
    /// Only files in this key.
    pub key: Option<Key>,
    /// Only files whose length in seconds is known and in this range.
    pub seconds: Option<RangeInclusive<f64>>,
    /// Only files in this folder or the folders beneath it.
    pub folder: Option<PathBuf>,
    /// Only files marked as favorites.
    pub favorites_only: bool,
    /// Also list audio files that cannot be decoded.
    pub include_unreadable: bool,
    /// Also list the recordings of sampled instruments: the hundreds of
    /// files that are the notes of one, which are otherwise left out so
    /// that a search for a sound finds sounds.
    pub include_recordings: bool,
    /// The most results wanted; `None` for all of them. A list on screen
    /// shows a few hundred rows, and asking for no more keeps searches that
    /// match everything as quick as those that match little.
    pub limit: Option<usize>,
}

impl Query {
    /// A query for `text` with no filters.
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Self::default() }
    }
}

/// How well one word of the query matches a file, best first. A file's
/// rank is the sum over the words, so that a word found in the name always
/// counts for more than one found only in the path, and a word found at the
/// start of the name or of a word in it for more than one buried inside.
mod class {
    pub const NAME_START: u8 = 6;
    pub const NAME_WORD: u8 = 5;
    pub const NAME_INSIDE: u8 = 4;
    /// The word names one of the file's tags, its tempo or its key.
    pub const TAG: u8 = 3;
    pub const PATH_WORD: u8 = 2;
    pub const PATH_INSIDE: u8 = 1;
}

/// Words of a query beyond this many are ignored. Nobody types more, and a
/// bound keeps a pasted paragraph from costing a pass per word.
const MAX_WORDS: usize = 12;

/// Marks an entry that failed to match some word.
const NO_MATCH: u16 = u16::MAX;

/// One word of a query, with what it can match besides text.
struct Word {
    text: String,
    tags: u64,
    tempo: Option<f32>,
    key: Option<u8>,
}

impl Word {
    fn new(word: &str) -> Self {
        let text = fold(word);
        let tempo = text.strip_suffix("bpm").and_then(|digits| digits.parse::<u16>().ok()).map(f32::from);
        // One letter is a search for that letter, not for a key.
        let key = (text.chars().count() >= 2).then(|| Key::parse(&text)).flatten().map(Key::code);
        Self { tags: tags_named(&text).bits(), tempo, key, text }
    }

    fn describes(&self, facts: &Facts) -> bool {
        facts.tags & self.tags != 0
            || self.tempo.is_some_and(|tempo| facts.tempo == tempo)
            || self.key.is_some_and(|key| facts.key == key)
    }
}

/// Lower case, the same way the index folds the text it searches.
fn fold(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// The tags a typed word refers to: by label or the start of one, so that
/// results appear while the word is still being typed, or by any word of
/// the vocabulary (`hh` finds hats however the files spell it).
fn tags_named(word: &str) -> Tags {
    VOCABULARY
        .iter()
        .filter(|rule| {
            is_word_or_plural(word, rule.label)
                || (word.len() >= 2 && rule.label.starts_with(word))
                || rule.words.iter().any(|entry| is_word_or_plural(word, entry))
        })
        .map(|rule| rule.tag)
        .collect()
}

/// The entries matching `query`, best first, and how many matched before
/// the limit was applied. `favorites` is consulted only if the query asks
/// for favorites.
pub(crate) fn search(index: &Index, query: &Query, favorites: &[EntryId]) -> (Vec<EntryId>, usize) {
    let scope = match &query.folder {
        Some(path) => match index.find_folder(path).and_then(|id| index.folder(id)) {
            Some(folder) => folder.all_files(),
            None => return (Vec::new(), 0),
        },
        None => 0..index.len(),
    };

    let mut words: Vec<Word> = Vec::new();
    for word in query.text.split_whitespace().take(MAX_WORDS).map(Word::new) {
        if !words.iter().any(|known| known.text == word.text) {
            words.push(word);
        }
    }

    // Each entry's rank so far, or NO_MATCH once a word has failed.
    let mut ranks = vec![0u16; scope.len()];
    let mut in_name = vec![0u8; scope.len()];
    let mut in_path = vec![0u8; index.folders.len()];
    for word in &words {
        in_name.fill(0);
        index.names.find(word.text.as_bytes(), scope.clone(), |entry, place| {
            in_name[entry - scope.start] = match place {
                MatchPlace::Start => class::NAME_START,
                MatchPlace::WordStart => class::NAME_WORD,
                MatchPlace::Inside => class::NAME_INSIDE,
            };
        });
        in_path.fill(0);
        index.folder_paths.find(word.text.as_bytes(), 0..index.folders.len(), |folder, place| {
            in_path[folder] = if place == MatchPlace::Inside { class::PATH_INSIDE } else { class::PATH_WORD };
        });
        let describes_something = word.tags != 0 || word.tempo.is_some() || word.key.is_some();

        for (offset, rank) in ranks.iter_mut().enumerate().filter(|(_, rank)| **rank != NO_MATCH) {
            let entry = scope.start + offset;
            let mut best = in_name[offset];
            if best == 0 {
                best = if describes_something && word.describes(&index.facts[entry]) {
                    class::TAG
                } else {
                    in_path[index.folder_of[entry] as usize]
                };
            }
            *rank = if best == 0 { NO_MATCH } else { *rank + u16::from(best) };
        }
    }

    let wanted_tags = query.tags.bits();
    let kind = query.kind.map(EntryKind::code);
    let key = query.key.map(Key::code);
    let tempo = query.tempo.clone();
    let seconds = query.seconds.as_ref().map(|range| *range.start() as f32..=*range.end() as f32);
    let passes = |facts: &Facts| {
        (query.include_unreadable || !facts.unreadable)
            && (query.include_recordings || !facts.part)
            && kind.is_none_or(|kind| facts.kind == kind)
            && facts.tags & wanted_tags == wanted_tags
            && key.is_none_or(|key| facts.key == key)
            // Unknown tempos and lengths are NaN, which no range contains.
            && tempo.as_ref().is_none_or(|range| range.contains(&facts.tempo))
            && seconds.as_ref().is_none_or(|range| range.contains(&facts.seconds))
    };
    // Sorting these puts the best rank first and, within a rank, keeps the
    // order of the tree, so equally good matches stay grouped by folder.
    let sort_key = |entry: usize| {
        let rank = ranks[entry - scope.start];
        (rank != NO_MATCH && passes(&index.facts[entry])).then(|| (u64::from(NO_MATCH - rank) << 32) | entry as u64)
    };
    let mut keys: Vec<u64> = if query.favorites_only {
        favorites.iter().map(|id| id.index()).filter(|entry| scope.contains(entry)).filter_map(sort_key).collect()
    } else {
        scope.clone().filter_map(sort_key).collect()
    };

    let total = keys.len();
    if let Some(limit) = query.limit
        && limit < keys.len()
    {
        if limit > 0 {
            keys.select_nth_unstable(limit);
        }
        keys.truncate(limit);
    }
    keys.sort_unstable();
    keys.dedup();
    (keys.into_iter().map(|key| EntryId(key as u32)).collect(), total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{AudioFacts, Entry};
    use crate::index::tests::{entry, index_of};
    use crate::infer::infer;
    use crate::tags::Tag;
    use std::path::Path;

    /// An entry with tempo, key and tags read from its path, and a length.
    fn sound(path: &str, seconds: f64) -> Entry {
        let mut entry = entry(path);
        let folders: Vec<&str> = Path::new(path).parent().unwrap().iter().map(|part| part.to_str().unwrap()).collect();
        let inferred = infer(&entry.name, &folders, Some(seconds));
        let frames = (seconds * 44_100.0).round() as u64;
        entry.audio = Some(AudioFacts { frames: Some(frames), sample_rate: 44_100, channels: 2 });
        (entry.tempo, entry.key, entry.tags) = (inferred.tempo, inferred.key, inferred.tags);
        entry
    }

    fn library() -> Index {
        index_of(vec![
            sound("Pack/Bass/Bass Drop.wav", 2.0),
            sound("Pack/Bass/Contrabass Solo.wav", 12.0),
            sound("Pack/Bass/Wild Bass.wav", 3.0),
            sound("Pack/Bass/Wobble.wav", 3.0),
            sound("Pack/Drums/Kicks/Hard Kick 01.wav", 0.4),
            sound("Pack/Drums/Kicks/Kick Soft.wav", 0.5),
            sound("Pack/Drums/Kicks/Thump.wav", 0.5),
            sound("Pack/Drums/Snares/Snare Tight.aif", 0.3),
            sound("Pack/Loops/Drum Loop 174bpm.wav", 240.0 / 174.0 * 2.0),
            sound("Pack/Loops/Keys Loop 140 BPM Am.wav", 240.0 / 140.0 * 4.0),
            sound("Pack/Loops/Melody Loop 140bpm C#m.flac", 240.0 / 140.0 * 8.0),
            entry("Pack/Midi/Bassline.mid"),
            sound("Other/HH Open 3.wav", 0.6),
            sound("Other/Kickflip Story.mp3", 95.0),
        ])
    }

    fn names(index: &Index, query: &Query) -> Vec<String> {
        search(index, query, &[]).0.into_iter().map(|id| index.entry(id).unwrap().name.clone()).collect()
    }

    fn found(index: &Index, text: &str) -> Vec<String> {
        names(index, &Query::text(text))
    }

    #[test]
    fn names_beat_paths_and_starts_beat_middles() {
        let index = library();
        // At the start of the name, at the start of a word in it, inside a
        // word, then only by tag or folder ("Wobble" lies in Bass).
        assert_eq!(found(&index, "bass"), ["Bass Drop", "Bassline", "Wild Bass", "Contrabass Solo", "Wobble"]);
        assert_eq!(found(&index, "kick"), ["Kick Soft", "Kickflip Story", "Hard Kick 01", "Thump"]);
        assert_eq!(found(&index, "BASS"), found(&index, "bass"), "case does not matter");
        assert_eq!(found(&index, "  kick  "), found(&index, "kick"));
    }

    #[test]
    fn every_word_must_match_somewhere() {
        let index = library();
        assert_eq!(found(&index, "kick hard"), ["Hard Kick 01"]);
        assert_eq!(found(&index, "hard kick"), ["Hard Kick 01"], "word order does not matter");
        assert_eq!(found(&index, "drums soft"), ["Kick Soft"], "one word in the name, one in a folder");
        assert_eq!(found(&index, "pack snare"), ["Snare Tight"]);
        assert_eq!(found(&index, "kick kick"), found(&index, "kick"), "a repeated word counts once");
        assert!(found(&index, "kick zebra").is_empty());
        assert!(found(&index, "zebra").is_empty());
        assert_eq!(found(&index, "wav hard"), ["Hard Kick 01"], "the extension is part of the name");
        assert_eq!(found(&index, "").len(), 14, "no words: everything");
    }

    #[test]
    fn words_find_tags_tempos_and_keys_however_files_spell_them() {
        let index = library();
        assert_eq!(found(&index, "hat"), ["HH Open 3"], "hh is a hat");
        assert_eq!(found(&index, "hats"), ["HH Open 3"]);
        assert_eq!(found(&index, "one-shot kick"), ["Kick Soft", "Hard Kick 01", "Thump"]);
        assert_eq!(found(&index, "140bpm"), ["Melody Loop 140bpm C#m", "Keys Loop 140 BPM Am"]);
        assert_eq!(found(&index, "c#m"), ["Melody Loop 140bpm C#m"]);
        assert_eq!(found(&index, "amin"), ["Keys Loop 140 BPM Am"], "the key, though the name spells it Am");
        assert_eq!(found(&index, "loo"), found(&index, "loop"), "a tag matches while it is being typed");
    }

    #[test]
    fn filters_narrow_the_results() {
        let index = library();
        let only = |change: &dyn Fn(&mut Query)| {
            let mut query = Query::default();
            change(&mut query);
            names(&index, &query)
        };
        assert_eq!(only(&|q| q.kind = Some(EntryKind::Midi)), ["Bassline"]);
        assert_eq!(only(&|q| q.kind = Some(EntryKind::Preset)), [""; 0]);
        assert_eq!(only(&|q| q.tags = Tags::of(&[Tag::Kick])), ["Hard Kick 01", "Kick Soft", "Thump"]);
        assert_eq!(only(&|q| q.tags = Tags::of(&[Tag::Drum, Tag::Loop])), ["Drum Loop 174bpm"]);
        assert_eq!(only(&|q| q.tags = Tags::of(&[Tag::Loop])).len(), 3);
        assert_eq!(only(&|q| q.tags = Tags::of(&[Tag::OneShot])).len(), 5);
        assert_eq!(only(&|q| q.tempo = Some(130.0..=150.0)), ["Keys Loop 140 BPM Am", "Melody Loop 140bpm C#m"]);
        assert_eq!(only(&|q| q.tempo = Some(174.0..=174.0)), ["Drum Loop 174bpm"]);
        assert_eq!(only(&|q| q.key = Key::parse("Am")), ["Keys Loop 140 BPM Am"]);
        assert_eq!(only(&|q| q.key = Key::parse("Db minor")), ["Melody Loop 140bpm C#m"], "the same key, respelled");
        assert_eq!(only(&|q| q.seconds = Some(10.0..=20.0)), ["Contrabass Solo", "Melody Loop 140bpm C#m"]);
        assert_eq!(only(&|q| q.seconds = Some(0.0..=0.45)), ["Hard Kick 01", "Snare Tight"]);
        assert_eq!(
            only(&|q| q.folder = Some("Pack/Drums".into())),
            ["Hard Kick 01", "Kick Soft", "Thump", "Snare Tight"]
        );
        assert_eq!(only(&|q| q.folder = Some("Other".into())), ["HH Open 3", "Kickflip Story"]);
        assert_eq!(only(&|q| q.folder = Some("Nowhere".into())), [""; 0]);
        // Filters combine with each other and with text.
        assert_eq!(
            only(&|q| {
                q.text = "loop".into();
                q.tempo = Some(100.0..=150.0);
                q.key = Key::parse("C#m");
                q.folder = Some("Pack".into());
            }),
            ["Melody Loop 140bpm C#m"]
        );
        assert_eq!(
            only(&|q| {
                q.text = "kick".into();
                q.folder = Some("Pack/Drums".into());
            }),
            ["Kick Soft", "Hard Kick 01", "Thump"]
        );
    }

    #[test]
    fn favorites_limits_and_unreadable_files() {
        let mut entries: Vec<Entry> = library().entries().to_vec();
        entries[1].unreadable = true;
        entries[1].audio = None;
        let index = index_of(entries);
        assert_eq!(
            found(&index, "bass"),
            ["Bass Drop", "Bassline", "Wild Bass", "Wobble"],
            "unreadable files stay out"
        );
        let with_unreadable = Query { text: "bass".into(), include_unreadable: true, ..Query::default() };
        assert_eq!(names(&index, &with_unreadable).len(), 5);

        let favorites = [
            index.find(Path::new("Pack/Bass/Wobble.wav")).unwrap(),
            index.find(Path::new("Other/HH Open 3.wav")).unwrap(),
        ];
        let favorite = |text: &str| {
            let query = Query { text: text.into(), favorites_only: true, ..Query::default() };
            search(&index, &query, &favorites)
                .0
                .into_iter()
                .map(|id| index.entry(id).unwrap().name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(favorite(""), ["Wobble", "HH Open 3"]);
        assert_eq!(favorite("bass"), ["Wobble"]);
        assert_eq!(favorite("kick"), [""; 0]);

        let limited = Query { text: "bass".into(), limit: Some(2), ..Query::default() };
        let (ids, total) = search(&index, &limited, &[]);
        assert_eq!((ids.len(), total), (2, 4), "the total counts every match");
        assert_eq!(names(&index, &limited), ["Bass Drop", "Bassline"], "the limit keeps the best");
        let none = Query { limit: Some(0), ..Query::default() };
        assert_eq!(search(&index, &none, &[]), (Vec::new(), 13));
    }

    #[test]
    fn odd_queries_are_harmless() {
        let index = library();
        assert!(found(&index, "\u{0}\n\t").len() <= 14);
        assert!(found(&index, &"x".repeat(10_000)).is_empty());
        assert!(found(&index, &"kick ".repeat(1_000)).len() == 4);
        assert!(found(&index, "💥 ünïcode ΣΊΣΥΦΟΣ").is_empty());
        assert!(found(&Index::default(), "kick").is_empty());
        let backwards = Query { tempo: Some(150.0..=130.0), ..Query::default() };
        assert!(names(&index, &backwards).is_empty());
        let nan = Query { seconds: Some(f64::NAN..=f64::NAN), ..Query::default() };
        assert!(names(&index, &nan).is_empty());
    }
}
