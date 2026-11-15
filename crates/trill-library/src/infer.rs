//! What a file's name and folders say about it: tempo, key and tags.
//!
//! Sample packs are named by people, each with their own conventions, so
//! everything here is a rule of thumb. The rules are tuned to be right when
//! they speak and silent when unsure: a wrong tempo or tag misleads, a
//! missing one only fails to help.
//!
//! The vocabulary of tags is the table in [`crate::tags`]; the numeric rules
//! are the constants below.

use crate::key::{Key, key_from_name};
use crate::tags::{Tag, Tags, VOCABULARY};
use std::collections::HashMap;
use std::sync::OnceLock;

/// The tempos a number marked `bpm` may state.
const TAGGED_BPM: std::ops::RangeInclusive<u32> = 40..=300;

/// The tempos a number standing alone in a name is taken for. Narrower than
/// [`TAGGED_BPM`], because bare numbers are also takes, years and sizes.
const BARE_BPM: std::ops::RangeInclusive<u32> = 60..=200;

/// Words after which a number is a model name, not a tempo: the SH-101, the
/// Juno-106, the CS-80, the MPC 60.
const MODEL_NAMES: &[&str] = &["sh", "juno", "cs", "mpc", "system", "jx"];

/// How many of the folders nearest a file may say what kind of sound it is.
/// Folders further up name packs and collections ("Bass Music Essentials"),
/// not their contents. Whether the files are loops or one-shots is read
/// from any depth: packs are divided that way near the top ("One Shots /
/// Synth / Multisampled / Analog Wavetable").
const DESCRIPTIVE_FOLDERS: usize = 3;

/// A sound shorter than this, in seconds, is a one-shot when nothing says
/// otherwise: too short for a bar at any ordinary tempo.
const ONE_SHOT_MAX_SECONDS: f64 = 1.5;

/// The longest loop, in bars, recognised by its length.
const MAX_LOOP_BARS: f64 = 64.0;

/// How far, in bars, a length may be from a whole number of bars and still
/// count as cut to the grid. Allows for a few milliseconds of padding.
const BAR_TOLERANCE: f64 = 0.02;

/// A word that ends in this is a loop whatever it begins with: `drumloop`,
/// `toploop`, `TEAROUTLOOP`.
const LOOP_SUFFIX: &str = "loop";

/// Pairs `(kept, dropped)`: where a name earns both tags, the second one is
/// describing the first rather than naming a sound of its own. A "lead
/// vocal" is a vocal; a "one shot" cut from a loop is a one-shot.
const DROPPED_BESIDE: &[(Tag, Tag)] = &[(Tag::Vocal, Tag::Lead), (Tag::OneShot, Tag::Loop)];

/// What was inferred about one file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inferred {
    /// Beats per minute.
    pub tempo: Option<f32>,
    pub key: Option<Key>,
    pub tags: Tags,
}

/// Infers tempo, key and tags for a file.
///
/// `stem` is the file name without its extension, `folders` the names of
/// the folders it lies in, outermost first (as far up as should be
/// considered, normally from the library root down), and `seconds` its
/// length if known, which settles loop or one-shot when the names do not.
pub fn infer(stem: &str, folders: &[&str], seconds: Option<f64>) -> Inferred {
    NameReading::of(stem, &FolderReading::of(folders)).finish(seconds)
}

/// What the folders a file lies in say about every file in them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FolderReading {
    /// What kind of sound the files are, used when a name does not say.
    sound: Tags,
    /// Whether the files are loops or one-shots, used likewise.
    form: Tags,
    tempo: Option<f32>,
}

impl FolderReading {
    /// Reads the names of the folders a file lies in, outermost first.
    pub(crate) fn of(folders: &[&str]) -> Self {
        let mut reading = Self::default();
        for (distance, folder) in folders.iter().rev().enumerate() {
            if reading.tempo.is_none() {
                reading.tempo = tempo_in(folder, false).map(|tempo| tempo.bpm);
            }
            // A folder that lists several things ("BASS & SOUNDS & LOOPS")
            // says nothing certain about any one file in it.
            if is_a_list(folder) {
                continue;
            }
            let tags = tags_in(&words(folder));
            if reading.sound.is_empty() && distance < DESCRIPTIVE_FOLDERS {
                reading.sound = tags.without(Tags::FORMS);
            }
            if reading.form.is_empty() {
                reading.form = tags.intersection(Tags::FORMS);
            }
        }
        reading
    }
}

fn is_a_list(folder: &str) -> bool {
    folder.contains(['&', ',', '+']) || words(folder).iter().any(|word| word == "and")
}

/// A tempo read from a name.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Tempo {
    bpm: f32,
    /// Read from a number standing alone rather than one marked `bpm`, and
    /// so open to doubt.
    bare: bool,
}

/// What one file's name says, before its length and its neighbours have
/// been taken into account.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NameReading {
    tags: Tags,
    tempo: Option<Tempo>,
    key: Option<Key>,
}

impl NameReading {
    pub(crate) fn of(stem: &str, folders: &FolderReading) -> Self {
        let named = tags_in(&words(stem));
        let (mut sound, mut form) = (named.without(Tags::FORMS), named.intersection(Tags::FORMS));
        // The name speaks first; folders fill in what it leaves unsaid.
        if sound.is_empty() {
            sound = folders.sound;
        }
        if form.is_empty() {
            form = folders.form;
        }
        let tempo = tempo_in(stem, true).or(folders.tempo.map(|bpm| Tempo { bpm, bare: false }));
        Self { tags: sound.union(form).with_implied(), tempo, key: key_from_name(stem) }
    }

    /// Completes the reading with the file's length, if it is known.
    pub(crate) fn finish(self, seconds: Option<f64>) -> Inferred {
        let Self { mut tags, mut tempo, key } = self;
        if !tags.intersects(Tags::FORMS)
            && let Some(seconds) = seconds
        {
            if tempo.is_some_and(|tempo| is_whole_bars(seconds, tempo.bpm)) {
                tags.insert(Tag::Loop);
            } else if seconds < ONE_SHOT_MAX_SECONDS {
                tags.insert(Tag::OneShot);
            }
        }
        // A bare number beside a single drum hit ("Kick 120", "Hat 184") is
        // a variation number. Only a pattern of hits has a tempo.
        let pattern = tags.contains(Tag::Loop) || tags.contains(Tag::Fill);
        if tempo.is_some_and(|tempo| tempo.bare) && tags.intersects(Tags::DRUM_HITS) && !pattern {
            tempo = None;
        }
        Inferred { tempo: tempo.map(|tempo| tempo.bpm), key, tags }
    }
}

/// Withdraws tempos read from numbers that are really counting.
///
/// `files` are the stems and readings of the files of one folder. Where
/// files differ only in their numbers and those numbers run on from one
/// another ("Vocal Chop 101", "102", "103"), the numbers are takes. Tempos
/// do not come in steps of one.
pub(crate) fn discount_counting(files: &mut [(&str, NameReading)]) {
    let mut families: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, (stem, reading)) in files.iter().enumerate() {
        if reading.tempo.is_some_and(|tempo| tempo.bare) {
            families.entry(shape(stem)).or_default().push(index);
        }
    }
    for members in families.values().filter(|members| members.len() >= 3) {
        let mut numbers: Vec<u32> =
            members.iter().filter_map(|&index| files[index].1.tempo).map(|tempo| tempo.bpm as u32).collect();
        numbers.sort_unstable();
        numbers.dedup();
        let counting = numbers.windows(3).any(|run| run[1] == run[0] + 1 && run[2] == run[1] + 1);
        if counting {
            members.iter().for_each(|&index| files[index].1.tempo = None);
        }
    }
}

/// A name with every run of digits replaced by `#`, so that names differing
/// only in their numbers compare equal.
fn shape(stem: &str) -> String {
    let mut shape = String::with_capacity(stem.len());
    for c in stem.chars() {
        let c = if c.is_ascii_digit() { '#' } else { c };
        if !(c == '#' && shape.ends_with('#')) {
            shape.push(c);
        }
    }
    shape
}

/// Whether `seconds` is a whole number of 4/4 bars at `bpm`.
fn is_whole_bars(seconds: f64, bpm: f32) -> bool {
    let bars = seconds * f64::from(bpm) / 240.0;
    let whole = bars.round();
    (1.0..=MAX_LOOP_BARS).contains(&whole) && (bars - whole).abs() <= BAR_TOLERANCE
}

/// The lower-case words of a name: runs of letters or digits, split again
/// where letters meet digits and at the humps of camelCase, so that
/// `OpenHat_02` reads "open", "hat", "02" and `FXLoop` reads "fx", "loop".
pub(crate) fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() && index > 0 {
            let previous = chars[index - 1];
            let next_is_lower = chars.get(index + 1).is_some_and(|next| next.is_lowercase());
            let boundary = previous.is_ascii_digit() != c.is_ascii_digit()
                || (previous.is_lowercase() && c.is_uppercase())
                // The last capital of a run starts the next word: "FXLoop".
                || (previous.is_uppercase() && c.is_uppercase() && next_is_lower);
            if boundary {
                words.push(std::mem::take(&mut current));
            }
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// The vocabulary arranged for lookup.
struct Lexicon {
    words: HashMap<&'static str, Tag>,
    /// Phrases as their words, longest first, so that the longest wins.
    phrases: Vec<(Vec<&'static str>, Tag)>,
}

fn lexicon() -> &'static Lexicon {
    static LEXICON: OnceLock<Lexicon> = OnceLock::new();
    LEXICON.get_or_init(|| {
        let mut lexicon = Lexicon { words: HashMap::new(), phrases: Vec::new() };
        for rule in VOCABULARY {
            for entry in rule.words {
                if entry.contains(' ') {
                    lexicon.phrases.push((entry.split(' ').collect(), rule.tag));
                } else {
                    lexicon.words.insert(entry, rule.tag);
                }
            }
        }
        lexicon.phrases.sort_by_key(|(phrase, _)| std::cmp::Reverse(phrase.len()));
        lexicon
    })
}

/// Whether `word` is `entry` or its plural.
pub(crate) fn is_word_or_plural(word: &str, entry: &str) -> bool {
    word == entry || word.strip_suffix('s') == Some(entry)
}

/// The tags the words of one name earn, before implications.
fn tags_in(words: &[String]) -> Tags {
    let lexicon = lexicon();
    let mut tags = Tags::EMPTY;
    let mut used = vec![false; words.len()];
    for (phrase, tag) in &lexicon.phrases {
        for start in 0..words.len().saturating_sub(phrase.len() - 1) {
            let window = &words[start..start + phrase.len()];
            let (last, leading) = (phrase.len() - 1, &phrase[..phrase.len() - 1]);
            let matches = !used[start..start + phrase.len()].iter().any(|used| *used)
                && window.iter().zip(leading).all(|(word, entry)| word == entry)
                && is_word_or_plural(&window[last], phrase[last]);
            if matches {
                tags.insert(*tag);
                used[start..start + phrase.len()].fill(true);
            }
        }
    }
    for (word, _) in words.iter().zip(&used).filter(|(_, used)| !**used) {
        let singular = word.strip_suffix('s').unwrap_or(word);
        if let Some(tag) = lexicon.words.get(word.as_str()).or_else(|| lexicon.words.get(singular)) {
            tags.insert(*tag);
        } else if singular.ends_with(LOOP_SUFFIX) {
            tags.insert(Tag::Loop);
        }
    }
    for (kept, dropped) in DROPPED_BESIDE {
        if tags.contains(*kept) {
            tags.remove(*dropped);
        }
    }
    tags
}

/// The tempo `text` states: a number marked `bpm` ("140bpm", "95 BPM",
/// "bpm_92") if there is one, and otherwise, if `allow_bare`, the first
/// number that stands alone and could be a tempo.
fn tempo_in(text: &str, allow_bare: bool) -> Option<Tempo> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let is_joiner = |byte: u8| matches!(byte, b' ' | b'_' | b'-');
    let mut bare = None;
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let digits = &lower[start..index];
        let Ok(number) = digits.parse::<u32>() else { continue };
        // Both slices end and start on ASCII, so slicing cannot split a char.
        let before = lower[..start].trim_end_matches(|c: char| c.is_ascii() && is_joiner(c as u8));
        let after = lower[index..].trim_start_matches(|c: char| c.is_ascii() && is_joiner(c as u8));
        if (after.starts_with("bpm") || before.ends_with("bpm")) && TAGGED_BPM.contains(&number) {
            return Some(Tempo { bpm: number as f32, bare: false });
        }
        if bare.is_none() && allow_bare && is_bare_tempo(&lower, start, index, number) {
            bare = Some(Tempo { bpm: number as f32, bare: true });
        }
    }
    bare
}

/// Whether the number at `start..end` of `lower` stands alone the way a
/// tempo does.
fn is_bare_tempo(lower: &str, start: usize, end: usize, number: u32) -> bool {
    let previous = lower[..start].chars().next_back();
    let mut following = lower[end..].chars();
    let (next, after_next) = (following.next(), following.next());
    let alone = !previous.is_some_and(char::is_alphanumeric) && !next.is_some_and(char::is_alphanumeric);
    // "078" is an index, "#120" a number in a series, "72-Audio" the name
    // of the track a recording came from.
    let index = lower[start..end].starts_with('0') || previous == Some('#');
    let track = next == Some('-') && after_next.is_some_and(char::is_alphabetic);
    let model = lower[..start]
        .trim_end_matches([' ', '_', '-'])
        .rsplit(|c: char| !c.is_alphanumeric())
        .next()
        .is_some_and(|word| MODEL_NAMES.contains(&word));
    alone && !index && !track && !model && BARE_BPM.contains(&number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(labels: &[&str]) -> Tags {
        labels.iter().map(|label| Tag::from_label(label).unwrap_or_else(|| panic!("no tag '{label}'"))).collect()
    }

    #[test]
    fn words_split_at_separators_digits_and_humps() {
        assert_eq!(words("OpenHat_02"), ["open", "hat", "02"]);
        assert_eq!(words("808Bass - Sub!"), ["808", "bass", "sub"]);
        assert_eq!(words("140BPM"), ["140", "bpm"]);
        assert_eq!(words("FXLoop"), ["fx", "loop"]);
        assert_eq!(words("Volt CleanCHat 02"), ["volt", "clean", "c", "hat", "02"]);
        assert_eq!(words("BISHU_kick_one_shot"), ["bishu", "kick", "one", "shot"]);
        assert_eq!(words("Überbaß Größe"), ["überbaß", "größe"]);
        assert!(words("  -- __ ").is_empty());
    }

    /// A table of real file names, with the folders they sit in and their
    /// lengths, against what should be read from them.
    #[test]
    fn realistic_names() {
        struct Case {
            path: &'static str,
            seconds: Option<f64>,
            tempo: Option<u32>,
            key: Option<&'static str>,
            tags: &'static [&'static str],
        }
        let case = |path, seconds, tempo, key, tags| Case { path, seconds, tempo, key, tags };
        let cases = [
            // Splice.
            case(
                "packs/Bishu Sample Pack/BISHU_labeled_sample_pack/BISHU_drums/BISHU_drum_one_shots/BISHU_kicks/BISHU_kick_one_shot_bishkick_01",
                Some(0.4),
                None,
                None,
                &["kick", "drum", "one-shot"],
            ),
            case(
                "packs/Bishu Sample Pack/BISHU_labeled_sample_pack/BISHU_fx/BISHU_fx_one_shot_lma_water_riser_150",
                Some(6.4),
                Some(150),
                None,
                &["fx", "riser", "one-shot"],
            ),
            case(
                "packs/Bishu Sample Pack/BISHU_labeled_sample_pack/BISHU_tonal/BISHU_synth/BISHU_synth_loops/BISHU_synth_loop_coolin_100_Fm",
                Some(9.6),
                Some(100),
                Some("Fm"),
                &["synth", "loop"],
            ),
            case(
                "packs/Bishu Sample Pack/BISHU_labeled_sample_pack/BISHU_vocals/BISHU_vocal_one_shots/BISHU_vocal_one_shot_huh_119_Am",
                Some(0.6),
                Some(119),
                Some("Am"),
                &["vocal", "one-shot"],
            ),
            case(
                "packs/Kitty Vocals & Stuff/KITTY_labeled_processed/KITTY_loops/KITTY_vocal_drum_loops/KITTY_vocal_percussion_loop_pew_pew_106",
                Some(4.5),
                Some(106),
                None,
                &["perc", "drum", "vocal", "loop"],
            ),
            case(
                "packs/Kitty Vocals & Stuff/KITTY_labeled_processed/KITTY_one_shots/KITTY_kits_music_box/KITTY_music_box_notes_B4",
                Some(2.0),
                None,
                None,
                &["bell", "one-shot"],
            ),
            case(
                "packs/Krysta Youngs Vocal Sample Pack/KRYSTA_YOUNGS_labeled_processed/KRYSTA_YOUNGS_vocals/KRYSTA_YOUNGS_vocal_fx/KRYSTA_YOUNGS_vocal_fx_transition_swell_rev_128_Fmaj",
                Some(3.75),
                Some(128),
                Some("F"),
                &["vocal", "fx", "riser", "loop"],
            ),
            case(
                "packs/Noizu Sample Pack Vol. 1/NOIZU_sample_pack/NOIZU_drums/NOIZU_claps/NOIZU_clap_loops/NOIZU_clap_loop_house_01_124",
                Some(3.87),
                Some(124),
                None,
                &["clap", "drum", "loop"],
            ),
            case(
                "packs/Noizu Sample Pack Vol. 1/NOIZU_sample_pack/NOIZU_drums/NOIZU_crashes/NOIZU_crash_delay_one_shot_04_124",
                Some(2.9),
                None,
                None,
                &["cymbal", "drum", "one-shot"],
            ),
            case(
                "packs/Noizu Sample Pack Vol. 1/NOIZU_sample_pack/NOIZU_drums/NOIZU_hihats/NOIZU_hihat_closed_one_shot_17",
                Some(0.2),
                None,
                None,
                &["hat", "drum", "one-shot"],
            ),
            case(
                "packs/Noizu Sample Pack Vol. 1/NOIZU_sample_pack/NOIZU_tonal/NOIZU_bass/NOIZU_bass_one_shots/NOIZU_bass_one_shot_moog_07_F",
                Some(1.9),
                None,
                None,
                &["bass", "one-shot"],
            ),
            case(
                "packs/Ultimate Gang Vocals/Ultimate_Gang_Vocals/loops/lead_vocal/dry/VOX_UGV_160_vocal_hook_gang_hey_power_post_lead_dry_A#min",
                Some(6.0),
                Some(160),
                Some("Bbm"),
                &["vocal", "loop"],
            ),
            case(
                "packs/Ultimate Gang Vocals/Ultimate_Gang_Vocals/oneshots/wet/VOX_UGV_vocal_gang_oneshot_yeah_gang_wet_Bmin",
                Some(1.1),
                None,
                Some("Bm"),
                &["vocal", "one-shot"],
            ),
            // Hand-named packs.
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/BASS & SOUNDS & LOOPS/BASSPOP MK1 - BEAUTY SINE W NOISE - 95 BPM F#M",
                Some(10.1),
                Some(95),
                Some("F#m"),
                &["fx", "loop"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/BASS & SOUNDS & LOOPS/BASSPOP MK1 - WILD BASS",
                Some(2.2),
                None,
                None,
                &["bass"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/BASS & SOUNDS & LOOPS/BASSPOP MK1 - 130 BASS FILL OF DOOM",
                Some(1.9),
                Some(130),
                None,
                &["bass", "fill"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/BASS & SOUNDS & LOOPS/BASSPOP MK1 - NICECOLORTRANSIENT",
                Some(0.3),
                None,
                None,
                &["one-shot"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/DRUM LOOPS/BASSPOP MK1 - TOM ROLL",
                Some(1.8),
                None,
                None,
                &["tom", "drum", "fill"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/DRUM LOOPS/BASSPOP MK1 - TRIPLET HATS 150",
                Some(3.2),
                Some(150),
                None,
                &["hat", "drum", "loop"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/DRUM LOOPS/BASSPOP MK1 - EZ BUILD",
                Some(6.0),
                None,
                None,
                &["drum", "loop"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/DRUM ONE SHOTS/HITS/BASSPOP MK1 - THE ONLY CLOSED HAT IN EXISTENCE",
                Some(0.1),
                None,
                None,
                &["hat", "drum", "one-shot"],
            ),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/BURST FEAT. PROP Project/Samples/Processed/Freeze/Freeze 73-Audio [2026-06-27 135125]-1",
                Some(42.0),
                None,
                None,
                &[],
            ),
            case("BASSPOP MULTIKIT VOL. 1 PREVIEW/BASSPOP MK1 - DRUMLOOP 5", Some(3.4), None, None, &["loop"]),
            case(
                "! ! BASSPOP MULTIKIT VOL. 1/BASS & SOUNDS & LOOPS/BASSPOP MK1 - TEAROUTLOOP",
                Some(3.3),
                None,
                None,
                &["loop"],
            ),
            case(
                "Packs/Drums/Kits/Jayce Lewis/JL Cymbals Overhead Ride Bell 02",
                Some(6.0),
                None,
                None,
                &["cymbal", "drum"],
            ),
            case("Packs/Drums/Percussion/Cow Bell 3", Some(0.4), None, None, &["perc", "drum", "one-shot"]),
            case("Packs/Instruments/Bells/Tubular Bell C4", Some(5.0), None, None, &["bell"]),
            case("BISHU - STARSTRUCK SAMPLE PACK/DRUM LOOP 2 - 174bpm", Some(5.6), Some(174), None, &["drum", "loop"]),
            case(
                "BISHU - STARSTRUCK SAMPLE PACK/MELODY 7 - 140bpm - Dm",
                Some(13.71),
                Some(140),
                Some("Dm"),
                &["loop"],
            ),
            case("BISHU - STARSTRUCK SAMPLE PACK/KICK 10", Some(0.5), None, None, &["kick", "drum", "one-shot"]),
            case(
                "Bishu Essentials Drum Pack Vol. 1/Cymbals/DubHat10",
                Some(0.3),
                None,
                None,
                &["hat", "drum", "one-shot"],
            ),
            case("Bishu Essentials Drum Pack Vol. 1/Hits/TrapHit10", Some(0.3), None, None, &["drum", "one-shot"]),
            case(
                "Bishu Essentials Drum Pack Vol. 1/Kicks/HouseKick8",
                Some(0.4),
                None,
                None,
                &["kick", "drum", "one-shot"],
            ),
            case("PATREON PACK MARCH 2026/convolv clap", Some(0.7), None, None, &["clap", "drum", "one-shot"]),
            // Ableton packs.
            case(
                "Beat Tools/Samples/One-Shots/Drums/Hihat/Hihat Closed Digi Phase",
                Some(0.2),
                None,
                None,
                &["hat", "drum", "one-shot"],
            ),
            case(
                "Build and Drop/Samples/Loops/Drum Loops/Full Loops/Mousehouse Stereoid E 128bpm",
                Some(7.5),
                Some(128),
                None,
                &["drum", "loop"],
            ),
            case(
                "Build and Drop/Samples/One Shots/Synth/Multisampled/Analog Wavetable/Analog Wavetable 075",
                Some(4.0),
                None,
                None,
                &["synth", "one-shot"],
            ),
            case(
                "Chop and Swing/Samples/Loops and Chords/Drum Loops/Perc Loops/Agogo Bells 1 90bpm",
                Some(5.33),
                Some(90),
                None,
                &["perc", "bell", "drum", "loop"],
            ),
            case(
                "Drone Lab/Samples/Long Shots/Bowed/Bowed Semi Hollow Baritone Guitar G#",
                Some(30.0),
                None,
                Some("Ab"),
                &["guitar"],
            ),
            case(
                "Drum Essentials/Samples/Drums/Conga/Conga Hi 505 Classic",
                Some(0.6),
                None,
                None,
                &["perc", "drum", "one-shot"],
            ),
            case("Latin Percussion/Samples/Combined/Shake-Caxixi 8 FX", Some(0.9), None, None, &["fx", "one-shot"]),
            case("Mood Reel/Samples/Synth Pads & Stabs/Processed Arp Loop", Some(8.0), None, None, &["arp", "loop"]),
            case(
                "Punch and Tilt/Samples/One Shots/Clap/Clap Slam FX 2",
                Some(0.8),
                None,
                None,
                &["clap", "drum", "fx", "one-shot"],
            ),
            case("Synth Essentials/Samples/Techno Pad/CS Techno Pad 95", Some(6.0), Some(95), None, &["pad"]),
            case(
                "Core Library/Samples/Loops/Drums/Full/Bouncy Beat 117 bpm",
                Some(8.2),
                Some(117),
                None,
                &["drum", "loop"],
            ),
            // FL Studio packs.
            case("Packs/Drums/Kicks/Grv Kick 19", Some(0.4), None, None, &["kick", "drum", "one-shot"]),
            case(
                "Packs/Drums/Kits/HQ Funk Kit/HH Open HQ Funk #4",
                Some(1.0),
                None,
                None,
                &["hat", "drum", "one-shot"],
            ),
            case("Packs/Drums/Kits/Jayce Lewis/JL Hat 184 Sizzle Hat 04", Some(2.5), None, None, &["hat", "drum"]),
            case("Packs/Drums/Kits/Rock/snare 41", Some(0.9), None, None, &["snare", "drum", "one-shot"]),
            case(
                "Packs/Drums (ModeAudio)/Hi Hats/Volt CleanCHat 02",
                Some(0.2),
                None,
                None,
                &["hat", "drum", "one-shot"],
            ),
            case(
                "Packs/Drums (ModeAudio)/Percussion/Attack Timbale 02",
                Some(0.5),
                None,
                None,
                &["perc", "drum", "one-shot"],
            ),
            case("Packs/Instruments/Keyboard/Grand/FL Piano (2)", Some(9.0), None, None, &["keys"]),
            case("Packs/Instruments/Guitar/Strat Pos B/Strat Soft 2 (1)", Some(5.0), None, None, &["guitar"]),
            case("Packs/Legacy/Drums/FPC/Cymbals/FPC_Ride_GLite_005", Some(3.0), None, None, &["cymbal", "drum"]),
            case("Packs/Risers/Riser Jetbike", Some(8.0), None, None, &["riser", "fx"]),
            case("Packs/Vocals/Laurie Webb Yeah", Some(1.2), None, None, &["vocal", "one-shot"]),
            // Assorted conventions.
            case("My Pack/Loops 128 BPM/Warm Pad", Some(15.0), Some(128), None, &["pad", "loop"]),
            case("My Pack/808s/Boomy", Some(1.9), None, None, &["808"]),
            case("My Pack/Bass Drum Big", Some(0.5), None, None, &["kick", "drum", "one-shot"]),
            case("My Pack/808 Kick", Some(0.5), None, None, &["808", "kick", "drum", "one-shot"]),
            case("My Pack/Synth Bass C", None, None, None, &["synth", "bass"]),
            case("My Pack/Juno 106 Strings", Some(4.0), None, None, &["strings"]),
            case("My Pack/SH-101 Lead", Some(2.0), None, None, &["lead"]),
            case("My Pack/Vocal 078", Some(2.0), None, None, &["vocal"]),
            case("My Pack/2MINUTES WOB", Some(120.0), None, None, &[]),
            case("My Pack/bpm_92_groove", Some(5.2), Some(92), None, &["drum", "loop"]),
            case("My Pack/hats of doom 140", Some(6.857), Some(140), None, &["hat", "drum", "loop"]),
            case("My Pack/hats of doom 140", Some(0.3), None, None, &["hat", "drum", "one-shot"]),
            case("My Pack/What Subtle Grime Thing", Some(3.0), None, None, &[]),
            case("My Pack/Lead Guitar Riff 90", Some(5.333), Some(90), None, &["lead", "guitar", "loop"]),
            case("My Pack/Kick 120", Some(0.4), None, None, &["kick", "drum", "one-shot"]),
        ];

        // Checked together, so that a change to a rule shows everything it
        // moved at once.
        let mut wrong = Vec::new();
        for Case { path, seconds, tempo, key, tags: expected } in cases {
            let mut parts: Vec<&str> = path.split('/').collect();
            let stem = parts.pop().unwrap();
            let inferred = infer(stem, &parts, seconds);
            let expected =
                Inferred { tempo: tempo.map(|bpm| bpm as f32), key: key.and_then(Key::parse), tags: tags(expected) };
            if inferred != expected {
                wrong.push(format!(
                    "{path}
     read: {inferred:?}
 expected: {expected:?}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of the names were read wrongly:
{}",
            wrong.len(),
            wrong.join(
                "
"
            )
        );
    }

    #[test]
    fn tempos_are_marked_or_stand_alone() {
        let tempo = |text: &str| tempo_in(text, true).map(|tempo| (tempo.bpm as u32, tempo.bare));
        assert_eq!(tempo("Melody 140bpm Cm"), Some((140, false)));
        assert_eq!(tempo("140 BPM"), Some((140, false)));
        assert_eq!(tempo("BPM 92 groove"), Some((92, false)));
        assert_eq!(tempo("Danzon 1 Guiro-112bpm"), Some((112, false)));
        assert_eq!(tempo("90S BOUNCE STABS - 95 BPM F#M"), Some((95, false)), "the mark beats the earlier number");
        assert_eq!(tempo("loop_95_"), Some((95, true)));
        assert_eq!(tempo("STRUT BASS 130"), Some((130, true)));
        assert_eq!(tempo("120 then 140"), Some((120, true)), "the first bare number");
        assert_eq!(tempo("Vocal 078"), None, "zero-padded: an index");
        assert_eq!(tempo("Hat #120"), None);
        assert_eq!(tempo("2MINUTES"), None, "glued to a word");
        assert_eq!(tempo("Freeze 72-Audio"), None, "a track name");
        assert_eq!(tempo("Loop 808 Bass 01"), None);
        assert_eq!(tempo("take 59"), None);
        assert_eq!(tempo("999bpm"), None, "out of range even when marked");
        assert_eq!(tempo("Juno-106 pad"), None);
        assert_eq!(tempo("99999999999999999999 140"), Some((140, true)), "absurd numbers are skipped");
        assert_eq!(tempo_in("Pack 2026 128", false), None, "folders need the mark");
        assert_eq!(tempo_in("Loops 128 BPM", false).map(|tempo| tempo.bpm), Some(128.0));
    }

    #[test]
    fn numbers_that_count_up_are_takes_not_tempos() {
        let folder = FolderReading::default();
        let stems = [
            "Vocal Chop 101",
            "Vocal Chop 102",
            "Vocal Chop 103",
            "Bass Loop 120",
            "Bass Loop 125",
            "Bass Loop 130",
            "Hook 140",
            "Hook 140 wet",
            "Pad 99",
            "Pad 100",
        ];
        let mut files: Vec<(&str, NameReading)> =
            stems.iter().map(|stem| (*stem, NameReading::of(stem, &folder))).collect();
        discount_counting(&mut files);
        let tempos: Vec<Option<f32>> = files.into_iter().map(|(_, reading)| reading.finish(None).tempo).collect();
        assert_eq!(
            tempos,
            [
                None,
                None,
                None,
                Some(120.0),
                Some(125.0),
                Some(130.0),
                Some(140.0),
                Some(140.0),
                Some(99.0),
                Some(100.0)
            ]
        );
    }

    #[test]
    fn length_settles_loop_or_one_shot_only_when_names_do_not() {
        let form = |stem: &str, folders: &[&str], seconds: f64| {
            infer(stem, folders, Some(seconds))
                .tags
                .intersection(Tags::FORMS)
                .iter()
                .map(Tag::label)
                .collect::<Vec<_>>()
        };
        // Four bars at 140 BPM, to the sample.
        assert_eq!(form("Melody 140", &[], 60.0 / 140.0 * 16.0), ["loop"]);
        assert_eq!(form("Melody 140", &[], 60.0 / 140.0 * 16.0 + 0.3), [""; 0], "a tail: not on the grid");
        assert_eq!(form("Melody", &[], 8.0), [""; 0], "no tempo, no evidence");
        assert_eq!(form("Thing", &[], 0.3), ["one-shot"]);
        assert_eq!(form("Thing", &[], 1.6), [""; 0]);
        // Names and folders are believed over length.
        assert_eq!(form("Long Riser One Shot 128", &[], 7.5), ["one-shot"]);
        assert_eq!(form("Tiny Loop", &[], 0.4), ["loop"]);
        assert_eq!(form("Thing", &["One Shots"], 7.5), ["one-shot"]);
        assert_eq!(form("Snare Fill 174", &["Loops"], 0.69), ["fill"]);
        assert_eq!(form("Thing", &["Loops", "Misc"], 0.2), ["loop"]);
        assert_eq!(form("Thing", &["One Shots", "A", "B", "C", "D"], 9.0), ["one-shot"], "however far up it is said");
        assert_eq!(form("Thing", &["Loops", "One Shots", "A"], 9.0), ["one-shot"], "the nearest folder that says");
        assert_eq!(infer("Thing", &[], None).tags, Tags::EMPTY);
    }

    #[test]
    fn folders_speak_only_when_the_name_is_silent_and_they_are_clear() {
        let sound = |stem: &str, folders: &[&str]| infer(stem, folders, None).tags;
        assert_eq!(sound("Thing 7", &["Pack", "Snares"]), tags(&["snare", "drum"]));
        assert_eq!(sound("Tight Clap", &["Pack", "Snares"]), tags(&["clap", "drum"]), "the name wins");
        assert_eq!(
            sound("Thing", &["Kicks", "Processed"]),
            tags(&["kick", "drum"]),
            "the nearest folder that says anything"
        );
        assert_eq!(sound("Thing", &["Bass & Leads"]), Tags::EMPTY, "a list is no description");
        assert_eq!(sound("Thing", &["Bass, Leads and Pads"]), Tags::EMPTY);
        assert_eq!(sound("Thing", &["Bass Music Essentials", "A", "B", "C"]), Tags::EMPTY, "too far up to describe it");
        assert_eq!(sound("Thing", &["Bass Music Essentials", "A", "B"]), tags(&["bass"]));
        assert_eq!(sound("", &[]), Tags::EMPTY);
    }
}
