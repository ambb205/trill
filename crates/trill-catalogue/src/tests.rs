//! The catalogue's own rules, on the one that is built in and on small
//! ones written here; and what a folder is checked against, on files the
//! tests write. Nothing is fetched and nothing of anyone's is read.

use super::check::{address, file_sum, text_sum, tree_sum, tree_sum_of};
use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;

const NO_FILES: &str = "format = 1\n";
const NOTHING_LEFT_OUT: &str = "format = 1\n";

fn sha(text: &str) -> String {
    text_sum(text.as_bytes())
}

/// A catalogue of two sets: an attribution piano fetched as an archive,
/// and public-domain strings fetched file by file.
fn small() -> (String, String) {
    let files = format!(
        "format = 1\n[list]\nstrings = '''\n{} 9 Violins.sfz\n{} 3 Samples/a 1.wav\n{} 3 Samples/b.wav\n'''\n",
        sha("<region>\n"),
        sha("aaa"),
        sha("bbb"),
    );
    let sum = tree_sum_of([("Grand.sfz", sha("piano").as_str()), ("Samples/c4.flac", sha("c4").as_str())]);
    let instruments = format!(
        r#"format = 1

[[set]]
id = "small-grand"
name = "Small Grand"
about = "A grand piano recorded in a small room."
by = "A. Maker"
licence = "CC-BY-4.0"
licence_at = "https://example.org/small-grand/LICENSE"
licence_says = "This work is licensed under a Creative Commons Attribution 4.0 International License."
credit = "Small Grand by A. Maker (CC BY 4.0)"
from = "https://example.org/small-grand"
version = "0123456789abcdef0123456789abcdef01234567"
read = "2026-11-17"
bytes = 7
files = 2
[set.fetch]
archive = "https://example.org/small-grand/archive/0123456789abcdef0123456789abcdef01234567.zip"
under = "small-grand-0123456789abcdef0123456789abcdef01234567/"
sum = "{sum}"

[[set.instrument]]
file = "Grand.sfz"
text = "{piano}"
name = "Small Grand"
kind = "piano"
words = "Grand piano, close and dry, four layers of loudness"
range = [21, 108]
layers = 4
takes = 1
recordings = 1
bytes = 2
noise = [70.0, 55.0]
tuning = [2.0, 3.0, 9.0]
evenness = [1.5, 4.0]
level = -12.0
marks = ["starter"]

[[set]]
id = "free-strings"
name = "Free Strings"
about = "A string section."
by = "B. Maker"
licence = "CC0-1.0"
licence_at = "https://example.org/free-strings/LICENSE"
licence_says = "This collection is under a Creative Commons 0 license."
from = "https://example.org/free-strings"
version = "89abcdef0123456789abcdef0123456789abcdef"
read = "2026-11-17"
bytes = 15
files = 3
[set.fetch]
base = "https://example.org/free-strings/raw/89abcdef0123456789abcdef0123456789abcdef/"
list = "strings"

[[set.instrument]]
file = "Violins.sfz"
text = "{violins}"
name = "Violins, sustained"
kind = "strings"
words = "Violin section, bowed and held"
range = [55, 96]
layers = 2
takes = 1
recordings = 2
bytes = 6
marks = ["noisy"]
misses = "It asks for 1 thing this player does not do."
note = "Noise is not given: a held note never falls quiet."
"#,
        piano = sha("piano"),
        violins = sha("<region>\n"),
    );
    (instruments, files)
}

fn read_small() -> Catalogue {
    let (instruments, files) = small();
    Catalogue::read(&instruments, &files, NOTHING_LEFT_OUT).unwrap()
}

#[test]
fn the_catalogue_that_is_built_in_reads_and_keeps_its_own_rules() {
    assert_eq!(Catalogue::problem(), None);
    let catalogue = Catalogue::shipped();
    assert!(catalogue.sets.len() >= 60, "{} sets", catalogue.sets.len());
    let mut ids = std::collections::BTreeSet::new();
    let mut texts = std::collections::BTreeMap::new();
    for set in &catalogue.sets {
        assert!(ids.insert(set.id.clone()), "{} is there twice", set.id);
        // Every licence was read somewhere and is quoted; a maker who asks to be named has a line.
        assert!(set.licence_at.starts_with("https://"), "{}", set.id);
        assert!(set.licence_says.len() >= 12, "{}: {}", set.id, set.licence_says);
        assert_eq!(set.licence.asks_credit(), set.credit.is_some(), "{}", set.id);
        // Pinned to a version, and fetched from where that version lies.
        assert_eq!(set.version.len(), 40, "{}: a commit", set.id);
        match &set.fetch {
            Fetch::Archive { url, under, sum } => {
                assert!(url.contains(&set.version) && under.contains(&set.version), "{}", set.id);
                assert_eq!(sum.len(), 64);
            }
            Fetch::Files { base, files } => {
                assert!(base.contains(&set.version), "{}", set.id);
                assert_eq!(files.len() as u32, set.files, "{}", set.id);
                assert_eq!(files.iter().map(|file| file.size).sum::<u64>(), set.bytes, "{}", set.id);
            }
        }
        assert!(set.bytes > 0 && set.files > 0, "{}", set.id);
        for instrument in &set.instruments {
            assert!(instrument.words.len() >= 12, "{} {}: {}", set.id, instrument.file, instrument.words);
            assert!(instrument.recordings > 0, "{} {}", set.id, instrument.file);
            if let Some((low, high)) = instrument.range {
                assert!(low <= high, "{} {}", set.id, instrument.file);
            }
            // One file is one instrument, wherever it is met: that is how it is known again.
            if let Some(other) = texts.insert(instrument.text.clone(), (set.id.clone(), instrument.file.clone())) {
                panic!("{} {} is the same file as {} {}", set.id, instrument.file, other.0, other.1);
            }
            // What is put first has nothing measured against it.
            assert!(!instrument.is_starter() || instrument.is_put_forward(), "{} {}", set.id, instrument.file);
        }
    }
    // What is fetched first is about a gigabyte, and has one of each kind a song commonly needs.
    let starter = catalogue.starter();
    let bytes: u64 = starter.iter().map(|set| set.bytes).sum();
    assert!((600 << 20..=1400 << 20).contains(&bytes), "the starter selection is {} MB", bytes >> 20);
    let kinds: Vec<Kind> =
        catalogue.instruments().filter(|(_, instrument)| instrument.is_starter()).map(|(_, i)| i.kind).collect();
    for kind in [Kind::Piano, Kind::ElectricPiano, Kind::Strings, Kind::Bass, Kind::DrumKit, Kind::Guitar, Kind::Organ]
    {
        assert!(kinds.contains(&kind), "no {} is fetched first", kind.id());
    }
    // What was left out says why, in the words that were read.
    assert!(!catalogue.left_out.is_empty());
    for out in &catalogue.left_out {
        assert!(!out.says.trim().is_empty() && out.from.starts_with("https://"), "{}", out.name);
    }
    for reason in [Reason::ShareAlike, Reason::NonCommercial, Reason::Unclear] {
        assert!(catalogue.left_out.iter().any(|out| out.reason == reason), "nothing was left out as {}", reason.id());
    }
}

#[test]
fn the_list_of_credits_in_the_repository_is_the_catalogue_s_line_for_line() {
    let written = include_str!("../../../catalogue/CREDITS.md");
    let listed: Vec<&str> = written.lines().filter_map(|line| line.strip_prefix("- ")).collect();
    let owed: Vec<String> = Catalogue::shipped().credits().into_iter().map(|credit| credit.line).collect();
    assert!(!owed.is_empty());
    for line in &owed {
        assert!(listed.contains(&line.as_str()), "catalogue/CREDITS.md does not have: {line}");
    }
    for line in &listed {
        assert!(
            owed.iter().any(|credit| credit == line),
            "catalogue/CREDITS.md names what the catalogue does not: {line}"
        );
    }
    // A line names its maker and its licence, so that it can stand alone wherever it is pasted.
    for set in Catalogue::shipped().sets.iter().filter(|set| set.licence.asks_credit()) {
        let line = set.credit.as_deref().unwrap();
        assert!(line.contains("CC BY"), "{}: {line}", set.id);
        assert!(line.contains("https://"), "{}: {line} does not say where its licence is", set.id);
    }
}

#[test]
fn a_catalogue_is_read_whole_or_not_at_all_and_says_which_entry_is_wrong() {
    let catalogue = read_small();
    assert_eq!(catalogue.sets.len(), 2);
    let grand = catalogue.set("small-grand").unwrap();
    assert_eq!((grand.licence, grand.is_starter(), grand.kinds()), (Licence::CcBy4, true, vec![Kind::Piano]));
    let strings = catalogue.set("free-strings").unwrap();
    let Fetch::Files { base, files } = &strings.fetch else { panic!("fetched file by file") };
    assert_eq!(files.len(), 3);
    assert_eq!(
        address(base, &files[1].path),
        "https://example.org/free-strings/raw/89abcdef0123456789abcdef0123456789abcdef/Samples/a%201.wav"
    );
    assert_eq!(
        address("https://e.org/", "Grand Piano, Steinway B/C#4 é.wav"),
        "https://e.org/Grand%20Piano%2C%20Steinway%20B/C%234%20%C3%A9.wav"
    );

    let (instruments, files) = small();
    let wrong = |from: &str, to: &str| {
        assert!(instruments.contains(from), "{from}");
        Catalogue::read(&instruments.replace(from, to), &files, NOTHING_LEFT_OUT).unwrap_err()
    };
    // A licence that does not let it be handed on is not one the catalogue takes.
    let error = wrong("licence = \"CC-BY-4.0\"", "licence = \"CC-BY-NC-4.0\"");
    assert_eq!(error, "the set 'small-grand': 'CC-BY-NC-4.0' is not a licence the catalogue takes");
    let error = wrong("licence = \"CC0-1.0\"", "licence = \"CC-BY-SA-4.0\"");
    assert!(error.contains("'CC-BY-SA-4.0' is not a licence the catalogue takes"), "{error}");
    // A maker who asks to be named has to have a line; a licence has to have been read somewhere.
    let error = wrong("credit = \"Small Grand by A. Maker (CC BY 4.0)\"\n", "");
    assert!(error.contains("asks that its maker be named, and it has no credit line"), "{error}");
    let error = wrong("licence_at = \"https://example.org/free-strings/LICENSE\"", "licence_at = \"somewhere\"");
    assert!(error.contains("does not say where its licence was read"), "{error}");
    // Fetched one way or the other; a list that is named is there; an instrument is among its set's files.
    let error = wrong("list = \"strings\"", "list = \"winds\"");
    assert_eq!(error, "the set 'free-strings': there is no list 'winds'");
    let error =
        wrong("base = \"https://example.org/free-strings/raw/89abcdef0123456789abcdef0123456789abcdef/\"\n", "");
    assert!(error.contains("as an archive (archive, under, sum) or file by file (base, list)"), "{error}");
    let error = wrong("file = \"Violins.sfz\"", "file = \"Violas.sfz\"");
    assert_eq!(error, "the set 'free-strings', 'Violas.sfz': it is not among the files its set is fetched as");
    let error = wrong("kind = \"strings\"", "kind = \"fiddles\"");
    assert!(error.contains("'fiddles' is not a kind of instrument"), "{error}");
    let error = wrong("marks = [\"noisy\"]", "marks = [\"lovely\"]");
    assert!(error.contains("'lovely' is not a mark"), "{error}");
    let error = wrong("id = \"free-strings\"", "id = \"Free Strings\"");
    assert!(error.contains("its id is not lower-case letters, digits and dashes"), "{error}");
    let error = wrong("id = \"free-strings\"", "id = \"small-grand\"");
    assert!(error.contains("there are two of it"), "{error}");
    let error = wrong("file = \"Grand.sfz\"", "file = \"../Grand.sfz\"");
    assert!(error.contains("its file or its checksum cannot be one"), "{error}");
    // A word nobody knows is a mistake, not something to pass over.
    let error = wrong("takes = 1\nrecordings = 1", "takes = 1\ntakse = 2\nrecordings = 1");
    assert!(error.starts_with("the catalogue cannot be read: "), "{error}");
    // Another format is another program's.
    let error = Catalogue::read(&instruments.replace("format = 1", "format = 2"), &files, NOTHING_LEFT_OUT);
    assert!(error.unwrap_err().contains("is of format 2, and this program reads format 1"));
    let outs = "format = 1\n[[left_out]]\nname = \"A Kit\"\nfrom = \"https://example.org/kit\"\nreason = \"share-alike\"\nsays = \"CC BY-SA 4.0\"\n";
    let catalogue = Catalogue::read(&instruments, &files, outs).unwrap();
    assert_eq!(catalogue.left_out[0].reason, Reason::ShareAlike);
    let error = Catalogue::read(&instruments, &files, &outs.replace("share-alike", "dislike")).unwrap_err();
    assert_eq!(error, "'A Kit' was left out for 'dislike', which is no reason");
    assert!(Catalogue::read(&instruments, NO_FILES, NOTHING_LEFT_OUT).is_err(), "a list that is named has to be there");
}

#[test]
fn instruments_are_found_by_their_words_and_what_was_measured_against_one_keeps_it_back() {
    let catalogue = read_small();
    let names = |search: Search| -> Vec<String> {
        catalogue.find(&search).iter().map(|(_, instrument)| instrument.name.clone()).collect()
    };
    // What something speaks against is offered, and not put forward.
    assert_eq!(names(Search::default()), ["Small Grand"]);
    assert_eq!(names(Search { with_faults: true, ..Search::default() }), ["Small Grand", "Violins, sustained"]);
    let all = |words: &str| names(Search { words: words.into(), with_faults: true, ..Search::default() });
    assert_eq!(all("GRAND dry"), ["Small Grand"], "every word, in any case");
    assert_eq!(all("b. maker"), ["Violins, sustained"], "by its maker");
    assert_eq!(all("strings bowed"), ["Violins, sustained"], "by its kind and its sentence");
    assert!(all("grand bowed").is_empty());
    assert_eq!(
        names(Search { kind: Some(Kind::Strings), with_faults: true, ..Search::default() }),
        ["Violins, sustained"]
    );
    assert_eq!(
        names(Search { public_domain_only: true, with_faults: true, ..Search::default() }),
        ["Violins, sustained"]
    );
    assert_eq!(names(Search { starter_only: true, with_faults: true, ..Search::default() }), ["Small Grand"]);
    assert_eq!(catalogue.starter().iter().map(|set| set.id.as_str()).collect::<Vec<_>>(), ["small-grand"]);
    assert_eq!((Kind::of("Drum-Kit"), Kind::of("drums")), (Some(Kind::DrumKit), None));
    assert!(Kind::ALL.iter().all(|kind| Kind::of(kind.id()) == Some(*kind) && !kind.name().is_empty()));
    assert!(Mark::ALL.iter().all(|mark| Mark::of(mark.id()) == Some(*mark) && !mark.words().is_empty()));
    assert!(Licence::ALL.iter().all(|licence| Licence::of(licence.id()) == Some(*licence)));
}

#[test]
fn a_song_owes_the_lines_of_the_makers_who_ask_to_be_named_each_once() {
    let catalogue = read_small();
    assert_eq!(
        catalogue.credits().iter().map(|credit| credit.line.as_str()).collect::<Vec<_>>(),
        ["Small Grand by A. Maker (CC BY 4.0)"],
        "what Trill offers that asks to be named"
    );
    let (piano, violins, other) = (sha("piano"), sha("<region>\n"), sha("something of the user's own"));
    let played = [
        ("Keys", piano.as_str()),
        ("Keys 2", piano.to_uppercase().leak() as &str),
        ("Pad", violins.as_str()),
        ("My Kit.sfz", other.as_str()),
        ("Keys", piano.as_str()),
    ];
    let owed = catalogue.owed(played);
    assert_eq!(owed.lines.len(), 1, "the maker is named once, however many tracks play the piano");
    let (credit, uses) = &owed.lines[0];
    assert_eq!((credit.name.as_str(), credit.licence), ("Small Grand", Licence::CcBy4));
    assert_eq!(uses, &["Small Grand (Keys)", "Small Grand (Keys 2)"]);
    assert_eq!(owed.free, ["Violins, sustained (Free Strings)"], "in the public domain: nothing is owed for it");
    assert_eq!(owed.unknown, ["My Kit.sfz"], "what the catalogue does not know is said, not guessed at");
    assert_eq!(owed.text(), "Small Grand by A. Maker (CC BY 4.0)");
    assert_eq!(catalogue.owed([]).text(), "");
    assert!(catalogue.by_text(&other).is_none());
}

fn write(folder: &Path, path: &str, text: &str) {
    let at = check::below(folder, path);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, text).unwrap();
}

#[test]
fn the_tree_checksum_is_the_checksum_of_a_line_for_each_file_in_the_order_of_their_paths() {
    let folder = tempfile::tempdir().unwrap();
    write(folder.path(), "b/c.txt", "two");
    write(folder.path(), "a.txt", "one");
    write(folder.path(), "B.txt", "three");
    // Not the set's: the record the fetching keeps, and what was left of a file when a fetch was stopped.
    write(folder.path(), "part.json", "{}");
    write(folder.path(), "b/d.flac.partial", "half");
    let lines = format!("{}  B.txt\n{}  a.txt\n{}  b/c.txt\n", sha("three"), sha("one"), sha("two"));
    let expected: String = Sha256::digest(lines.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    let tree = tree_sum(folder.path()).unwrap();
    assert_eq!((tree.sum.as_str(), tree.files, tree.bytes), (expected.as_str(), 3, 11));
    let known = [("b/c.txt", sha("two")), ("B.txt", sha("three")), ("a.txt", sha("one"))];
    assert_eq!(tree_sum_of(known.iter().map(|(path, sum)| (*path, sum.as_str()))), expected, "without reading them");
    assert_eq!(file_sum(&folder.path().join("a.txt")).unwrap(), sha("one"));
    assert_eq!(sha(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    let error = tree_sum(&folder.path().join("nowhere")).unwrap_err();
    assert!(error.starts_with("there is no folder at "), "{error}");
}

#[test]
fn a_set_that_arrived_is_whole_and_as_it_was_read_or_a_sentence_says_what_is_not() {
    let catalogue = read_small();
    let (grand, strings) = (catalogue.set("small-grand").unwrap(), catalogue.set("free-strings").unwrap());
    let folder = tempfile::tempdir().unwrap();

    // Fetched as an archive: what it unpacked to is what was read.
    let at = folder.path().join("small-grand");
    assert!(grand.check(&at).unwrap_err().contains("there is no such folder"));
    write(&at, "Grand.sfz", "piano");
    let error = grand.check(&at).unwrap_err();
    assert!(error.contains("it has 1 files where the set has 2: it was not unpacked to the end"), "{error}");
    write(&at, "Samples/c4.flac", "c4");
    write(&at, "part.json", "{}");
    let whole = grand.check(&at).unwrap();
    assert_eq!((whole.files, whole.bytes), (2, 7));
    assert_eq!(whole.instruments, [at.join("Grand.sfz")]);
    write(&at, "Samples/c4.flac", "d4");
    let error = grand.check(&at).unwrap_err();
    assert!(
        error.starts_with("Small Grand at ") && error.contains("its files are other files than were read"),
        "{error}"
    );
    assert!(error.contains("Remove it and fetch it again"), "{error}");
    write(&at, "Samples/c4.flac", "c4");
    write(&at, "notes of my own.txt", "mine");
    let error = grand.check(&at).unwrap_err();
    assert!(error.contains("it has 3 files where the set has 2: something else is in its folder"), "{error}");

    // Fetched file by file: a fetch that was stopped half way is said, and so is a file that is another.
    let at = folder.path().join("free-strings");
    write(&at, "Violins.sfz", "<region>\n");
    write(&at, "Samples/a 1.wav", "aaa");
    write(&at, "Samples/b.wav.partial", "b");
    let error = strings.check(&at).unwrap_err();
    assert!(error.contains("1 of its 3 files are not there (Samples/b.wav)"), "{error}");
    assert!(error.contains("fetch it again, and what is there is kept"), "{error}");
    write(&at, "Samples/b.wav", "bbb");
    let whole = strings.check(&at).unwrap();
    assert_eq!((whole.files, whole.bytes, whole.instruments.len()), (3, 15, 1));
    // What else lies in its folder is none of its business.
    write(&at, "readme of my own.txt", "mine");
    assert!(strings.check(&at).is_ok());
    write(&at, "Samples/b.wav", "bbc");
    let error = strings.check(&at).unwrap_err();
    assert!(error.contains("1 of its 3 files are other files than were read (Samples/b.wav)"), "{error}");
    write(&at, "Samples/b.wav", "bb");
    assert!(strings.check(&at).unwrap_err().contains("other files than were read"), "another size is another file");
}
