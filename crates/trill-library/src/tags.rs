//! The tag vocabulary: what kinds of sound the library knows by name.
//!
//! [`VOCABULARY`] is the whole of it. Adding a word to a tag, or a new tag,
//! is an edit to that table (and to [`Tag`] for a new tag); nothing else
//! needs to know.

/// A kind of sound, or a form a sound takes.
///
/// Tags are inferred from file and folder names. A file can carry several:
/// a sub-bass one-shot is `Bass` and `OneShot`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tag {
    // Drums.
    Kick,
    Snare,
    Clap,
    Hat,
    Tom,
    Cymbal,
    Perc,
    /// Any drum or percussion sound; implied by the specific drum tags, and
    /// what drum loops and breaks are tagged with.
    Drum,
    // Tonal.
    /// The 808 family: sub-bass slides and TR-808 drums alike.
    Sub808,
    Bass,
    Lead,
    Pad,
    Pluck,
    Keys,
    Guitar,
    Synth,
    Strings,
    Brass,
    Bell,
    Chord,
    Arp,
    Stab,
    Vocal,
    // Effects.
    Fx,
    Riser,
    Impact,
    Atmosphere,
    Foley,
    // Form.
    Loop,
    OneShot,
    Fill,
}

/// One row of the vocabulary: a tag, how it is shown, and the words that
/// mean it.
pub struct TagRule {
    pub tag: Tag,
    /// The name shown to the user and matched by searches, lower case.
    pub label: &'static str,
    /// Lower-case words that mean this tag when they appear in a file or
    /// folder name. A plural `s` is accepted on any of them. An entry with
    /// a space is a phrase of consecutive words; phrases are matched before
    /// single words and use their words up, which is how "bass drum" comes
    /// to mean a kick and not a bass.
    pub words: &'static [&'static str],
    /// Tags that follow from this one.
    pub implies: &'static [Tag],
}

/// Every tag, in the order they are shown.
pub const VOCABULARY: &[TagRule] = &[
    TagRule {
        tag: Tag::Kick,
        label: "kick",
        words: &["kick", "kik", "kck", "bd", "kickdrum", "bassdrum", "kick drum", "bass drum"],
        implies: &[Tag::Drum],
    },
    TagRule {
        tag: Tag::Snare,
        label: "snare",
        words: &["snare", "snr", "sd", "snaredrum", "snare drum"],
        implies: &[Tag::Drum],
    },
    TagRule { tag: Tag::Clap, label: "clap", words: &["clap", "clp", "handclap", "hand clap"], implies: &[Tag::Drum] },
    TagRule {
        tag: Tag::Hat,
        label: "hat",
        words: &[
            "hat",
            "hihat",
            "hh",
            "hhat",
            "ohat",
            "chat",
            "ohh",
            "chh",
            "openhat",
            "closedhat",
            "hi hat",
            "high hat",
        ],
        implies: &[Tag::Drum],
    },
    TagRule { tag: Tag::Tom, label: "tom", words: &["tom", "floortom", "floor tom"], implies: &[Tag::Drum] },
    TagRule {
        tag: Tag::Cymbal,
        label: "cymbal",
        words: &["cymbal", "cym", "crash", "ride", "splash", "china", "ride bell"],
        implies: &[Tag::Drum],
    },
    TagRule {
        tag: Tag::Perc,
        label: "perc",
        words: &[
            "perc",
            "percussion",
            "shaker",
            "tambourine",
            "tamb",
            "conga",
            "bongo",
            "cowbell",
            "cow bell",
            "clave",
            "rim",
            "rimshot",
            "sidestick",
            "snap",
            "woodblock",
            "triangle",
            "timbale",
            "agogo",
            "guiro",
            "cabasa",
            "maraca",
            "djembe",
            "tabla",
        ],
        implies: &[Tag::Drum],
    },
    TagRule {
        tag: Tag::Drum,
        label: "drum",
        words: &["drum", "drumkit", "beat", "break", "breakbeat", "drumbreak", "groove"],
        implies: &[],
    },
    TagRule { tag: Tag::Sub808, label: "808", words: &["808"], implies: &[] },
    TagRule { tag: Tag::Bass, label: "bass", words: &["bass", "bassline", "sub", "subbass", "reese"], implies: &[] },
    TagRule { tag: Tag::Lead, label: "lead", words: &["lead"], implies: &[] },
    TagRule { tag: Tag::Pad, label: "pad", words: &["pad"], implies: &[] },
    TagRule { tag: Tag::Pluck, label: "pluck", words: &["pluck"], implies: &[] },
    TagRule {
        tag: Tag::Keys,
        label: "keys",
        words: &[
            "keys",
            "piano",
            "rhodes",
            "organ",
            "epiano",
            "wurlitzer",
            "wurli",
            "clavinet",
            "harpsichord",
            "keyboard",
            "electric piano",
        ],
        implies: &[],
    },
    TagRule { tag: Tag::Guitar, label: "guitar", words: &["guitar", "gtr"], implies: &[] },
    TagRule { tag: Tag::Synth, label: "synth", words: &["synth", "synthesizer", "supersaw"], implies: &[] },
    TagRule {
        tag: Tag::Strings,
        label: "strings",
        words: &["strings", "string", "violin", "viola", "cello", "orchestral"],
        implies: &[],
    },
    TagRule { tag: Tag::Brass, label: "brass", words: &["brass", "horn", "trumpet", "trombone"], implies: &[] },
    TagRule {
        tag: Tag::Bell,
        label: "bell",
        words: &[
            "bell",
            "glock",
            "glockenspiel",
            "chime",
            "kalimba",
            "marimba",
            "vibraphone",
            "xylophone",
            "music box",
        ],
        implies: &[],
    },
    TagRule { tag: Tag::Chord, label: "chord", words: &["chord"], implies: &[] },
    TagRule { tag: Tag::Arp, label: "arp", words: &["arp", "arpeggio"], implies: &[] },
    TagRule { tag: Tag::Stab, label: "stab", words: &["stab"], implies: &[] },
    TagRule {
        tag: Tag::Vocal,
        label: "vocal",
        words: &[
            "vocal",
            "vox",
            "voice",
            "acapella",
            "acappella",
            "adlib",
            "chant",
            "choir",
            "spoken",
            "lead vocal",
            "lead vox",
            "ad lib",
        ],
        implies: &[],
    },
    TagRule {
        tag: Tag::Fx,
        label: "fx",
        words: &[
            "fx",
            "sfx",
            "effect",
            "glitch",
            "laser",
            "zap",
            "siren",
            "whoosh",
            "sweep",
            "transition",
            "downlifter",
            "downsweep",
            "noise",
        ],
        implies: &[],
    },
    TagRule {
        tag: Tag::Riser,
        label: "riser",
        words: &["riser", "rise", "uplifter", "upsweep", "buildup", "swell", "build up"],
        implies: &[Tag::Fx],
    },
    TagRule { tag: Tag::Impact, label: "impact", words: &["impact", "explosion"], implies: &[Tag::Fx] },
    TagRule {
        tag: Tag::Atmosphere,
        label: "atmosphere",
        words: &["atmosphere", "atmos", "ambience", "ambiance", "ambient", "texture", "drone", "soundscape"],
        implies: &[],
    },
    TagRule { tag: Tag::Foley, label: "foley", words: &["foley"], implies: &[] },
    TagRule { tag: Tag::Loop, label: "loop", words: &["loop", "looped"], implies: &[] },
    TagRule { tag: Tag::OneShot, label: "one-shot", words: &["oneshot", "hit", "one shot"], implies: &[] },
    TagRule { tag: Tag::Fill, label: "fill", words: &["fill", "drumfill", "roll"], implies: &[] },
];

impl Tag {
    /// The name shown to the user: `kick`, `808`, `one-shot`.
    pub fn label(self) -> &'static str {
        self.rule().map_or("", |rule| rule.label)
    }

    /// The tag with this label, in any letter case.
    pub fn from_label(label: &str) -> Option<Tag> {
        VOCABULARY.iter().find(|rule| rule.label.eq_ignore_ascii_case(label)).map(|rule| rule.tag)
    }

    /// Every tag, in the order they are shown.
    pub fn all() -> impl Iterator<Item = Tag> {
        VOCABULARY.iter().map(|rule| rule.tag)
    }

    fn rule(self) -> Option<&'static TagRule> {
        VOCABULARY.iter().find(|rule| rule.tag == self)
    }

    fn bit(self) -> u64 {
        1 << self as u8
    }
}

/// A set of [`Tag`]s.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct Tags(u64);

impl Tags {
    pub const EMPTY: Tags = Tags(0);

    /// The sounds one drum hit can be. A bare number beside one of these is
    /// a variation number, not a tempo.
    pub(crate) const DRUM_HITS: Tags =
        Tags::of(&[Tag::Kick, Tag::Snare, Tag::Clap, Tag::Hat, Tag::Tom, Tag::Cymbal, Tag::Perc]);

    /// How a sound is meant to be used rather than what it is.
    pub(crate) const FORMS: Tags = Tags::of(&[Tag::Loop, Tag::OneShot, Tag::Fill]);

    /// The set holding exactly `tags`.
    pub const fn of(tags: &[Tag]) -> Tags {
        let mut bits = 0;
        let mut index = 0;
        while index < tags.len() {
            bits |= 1 << tags[index] as u8;
            index += 1;
        }
        Tags(bits)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub fn contains(self, tag: Tag) -> bool {
        self.0 & tag.bit() != 0
    }

    /// Whether every tag of `other` is in this set.
    pub fn contains_all(self, other: Tags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the sets share a tag.
    pub fn intersects(self, other: Tags) -> bool {
        self.0 & other.0 != 0
    }

    pub fn insert(&mut self, tag: Tag) {
        self.0 |= tag.bit();
    }

    pub fn remove(&mut self, tag: Tag) {
        self.0 &= !tag.bit();
    }

    #[must_use]
    pub fn union(self, other: Tags) -> Tags {
        Tags(self.0 | other.0)
    }

    #[must_use]
    pub fn intersection(self, other: Tags) -> Tags {
        Tags(self.0 & other.0)
    }

    /// The tags of this set that are not in `other`.
    #[must_use]
    pub fn without(self, other: Tags) -> Tags {
        Tags(self.0 & !other.0)
    }

    /// The tags in display order.
    pub fn iter(self) -> impl Iterator<Item = Tag> {
        Tag::all().filter(move |tag| self.contains(*tag))
    }

    /// This set plus everything its tags imply.
    #[must_use]
    pub(crate) fn with_implied(self) -> Tags {
        let mut all = self;
        for rule in VOCABULARY.iter().filter(|rule| self.contains(rule.tag)) {
            all = all.union(Tags::of(rule.implies));
        }
        all
    }

    /// The set as stored in the index file.
    pub(crate) fn bits(self) -> u64 {
        self.0
    }

    /// The inverse of [`Tags::bits`]; bits that are not tags are dropped.
    pub(crate) fn from_bits(bits: u64) -> Tags {
        // Tags are numbered in the order of the vocabulary, from zero.
        Tags(bits & ((1 << VOCABULARY.len()) - 1))
    }
}

impl FromIterator<Tag> for Tags {
    fn from_iter<I: IntoIterator<Item = Tag>>(tags: I) -> Self {
        let mut set = Tags::EMPTY;
        tags.into_iter().for_each(|tag| set.insert(tag));
        set
    }
}

impl std::fmt::Debug for Tags {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_set().entries(self.iter().map(Tag::label)).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_is_consistent() {
        // One row per tag, in the order of the enum, so the table is the
        // single description of the vocabulary.
        for (index, rule) in VOCABULARY.iter().enumerate() {
            assert_eq!(rule.tag as usize, index, "{} is out of order", rule.label);
            assert_eq!(rule.label, rule.label.to_lowercase());
            assert_eq!(Tag::from_label(&rule.label.to_uppercase()), Some(rule.tag));
            assert_eq!(rule.tag.label(), rule.label);
            for word in rule.words {
                assert_eq!(*word, word.to_lowercase(), "words are lower case");
                assert!(!word.is_empty() && word.split(' ').all(|part| !part.is_empty()), "'{word}'");
            }
        }
        assert!(VOCABULARY.len() <= 64, "tags are stored as bits of a u64");
        assert_eq!(Tag::from_label("nonsense"), None);

        // No word means two tags, except through a longer phrase.
        let mut seen = std::collections::HashMap::new();
        for rule in VOCABULARY {
            for word in rule.words {
                assert_eq!(seen.insert(*word, rule.label), None, "'{word}' is listed twice");
            }
        }
    }

    #[test]
    fn sets_behave_like_sets() {
        let mut tags = Tags::of(&[Tag::Kick, Tag::OneShot]);
        assert!(tags.contains(Tag::Kick) && !tags.contains(Tag::Snare));
        assert_eq!(tags.len(), 2);
        tags.insert(Tag::Sub808);
        tags.remove(Tag::OneShot);
        assert_eq!(tags.iter().collect::<Vec<_>>(), [Tag::Kick, Tag::Sub808]);
        assert_eq!(format!("{tags:?}"), r#"{"kick", "808"}"#);
        assert!(tags.contains_all(Tags::of(&[Tag::Kick])) && !tags.contains_all(Tags::of(&[Tag::Kick, Tag::Loop])));
        assert!(tags.intersects(Tags::DRUM_HITS) && !tags.intersects(Tags::FORMS));
        assert_eq!(tags.without(Tags::DRUM_HITS), Tags::of(&[Tag::Sub808]));
        assert_eq!(tags.intersection(Tags::DRUM_HITS), Tags::of(&[Tag::Kick]));
        assert_eq!([Tag::Pad, Tag::Loop].into_iter().collect::<Tags>(), Tags::of(&[Tag::Loop, Tag::Pad]));
        assert!(Tags::EMPTY.is_empty());
    }

    #[test]
    fn implications_and_storage() {
        assert_eq!(Tags::of(&[Tag::Kick]).with_implied(), Tags::of(&[Tag::Kick, Tag::Drum]));
        assert_eq!(Tags::of(&[Tag::Riser, Tag::Pad]).with_implied(), Tags::of(&[Tag::Riser, Tag::Pad, Tag::Fx]));
        let tags = Tags::of(&[Tag::Fill, Tag::Vocal]);
        assert_eq!(Tags::from_bits(tags.bits()), tags);
        assert_eq!(Tags::from_bits(u64::MAX).len(), VOCABULARY.len(), "unknown bits are dropped");
    }
}
