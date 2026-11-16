//! Throws random and near-valid text at every notation the crate reads.
//! Whatever comes in, the answer is a value or an error with a message:
//! never a panic, and never a note the project model would reject.

use trill_core::{Note, TimeSignature};
use trill_theory::arpeggio::{ArpOrder, Arpeggio};
use trill_theory::bass::{BassLine, BassStyle, bass_line};
use trill_theory::drums::DrumPattern;
use trill_theory::melody::Phrase;
use trill_theory::progression::{Comping, Progression, Rhythm, comp};
use trill_theory::roman;
use trill_theory::timing::{note_value, parse_beats};
use trill_theory::voicing::{Bass, VoicingSpec, VoicingStyle, voice};
use trill_theory::{Chord, Rng, Scale, StepPattern, estimate_key, identify, read_chords, summarize};

/// Characters the notations use, plus some they do not.
const ALPHABET: &[&str] = &[
    "A", "B", "C", "D", "E", "F", "G", "H", "a", "b", "c", "d", "g", "i", "j", "m", "M", "n", "o", "O", "r", "s", "t",
    "u", "v", "x", "X", "I", "V", "#", "b", "♭", "♯", "°", "ø", "Δ", "^", "+", "-", "/", "(", ")", ",", ".", ":", "|",
    "%", "~", "_", "'", " ", " ", "0", "1", "2", "3", "4", "5", "6", "7", "9", "11", "13", "maj", "min", "dim", "aug",
    "sus", "add", "no", "alt", "N.C.", "é", "🎹", "\t", "\n", "",
];

/// Inputs that are valid somewhere, to be mangled.
const SEEDS: &[&str] = &[
    "Am9",
    "Fmaj7/A",
    "C#m7b5",
    "G7sus4",
    "Bb6/9",
    "Dadd9",
    "E7#9b13",
    "Cdim7",
    "F#aug",
    "N.C.",
    "CmMaj7",
    "C7alt",
    "ii7",
    "V7/V",
    "bVII",
    "vii°7",
    "iiø65",
    "I64",
    "#iv°",
    "Am9 | Fmaj7 | C G/B |",
    "Dm7 . . G7 | Cmaj7 | % | N.C.",
    "C:3 G:1 | F:1.5 G:2.5",
    "ii7 V7 | Imaj7",
    "x...x...x...x...",
    "X.x.X.x.|x.xx.xx.xx.x",
    "x.x.x.x.x.x.x3_x.",
    "x__x__x_",
    "g.g.x.x.xxxxX2X2X2X2",
    ":0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4",
    "E5:0.5 F#5 ~ r:2 Bb3",
    "1 b3 #4 5' 7,, ~:0.33",
    "A minor",
    "F# dorian",
    "Bbm",
    "C whole tone",
    "eb major pentatonic",
    "1/8t",
    "0.25",
    "tresillo",
    "up_down",
];

fn random_text(rng: &mut Rng) -> String {
    if rng.below(3) == 0 {
        // Wholly random.
        (0..rng.below(24)).map(|_| ALPHABET[rng.below(ALPHABET.len())]).collect()
    } else {
        // A valid input with a few characters inserted, removed or replaced.
        let mut chars: Vec<String> = SEEDS[rng.below(SEEDS.len())].chars().map(String::from).collect();
        for _ in 0..1 + rng.below(4) {
            let at = rng.below(chars.len() + 1);
            match rng.below(3) {
                0 => chars.insert(at, ALPHABET[rng.below(ALPHABET.len())].to_string()),
                1 if at < chars.len() => {
                    chars.remove(at);
                }
                _ if at < chars.len() => chars[at] = ALPHABET[rng.below(ALPHABET.len())].to_string(),
                _ => {}
            }
        }
        chars.concat()
    }
}

fn signature(rng: &mut Rng) -> TimeSignature {
    let choices = [(4, 4), (3, 4), (6, 8), (7, 8), (5, 4), (12, 8), (2, 2), (9, 16), (1, 4), (32, 16)];
    let (numerator, denominator) = choices[rng.below(choices.len())];
    TimeSignature::new(numerator, denominator).unwrap()
}

fn spec(rng: &mut Rng) -> VoicingSpec {
    VoicingSpec {
        style: VoicingStyle::ALL[rng.below(VoicingStyle::ALL.len())],
        low: rng.below(128) as u8,
        high: rng.below(128) as u8,
        voices: rng.below(9),
        bass: [Bass::None, Bass::Root, Bass::Octaves][rng.below(3)],
        bass_low: rng.below(128) as u8,
        inversion: [None, Some(rng.below(9))][rng.below(2)],
    }
}

/// What the project model requires of a note.
fn assert_playable(notes: &[Note], context: &str) {
    for note in notes {
        assert!(note.pitch <= 127, "{context}: pitch {}", note.pitch);
        assert!((1..=127).contains(&note.velocity), "{context}: velocity {}", note.velocity);
        assert!(note.start.is_finite() && note.start >= 0.0, "{context}: start {}", note.start);
        assert!(note.length.is_finite() && note.length > 0.0, "{context}: length {}", note.length);
    }
}

fn assert_explains(error: &trill_theory::Error, text: &str) {
    assert!(error.message().len() > 10, "an error for {text:?} says too little: {error}");
}

#[test]
fn no_input_panics_and_everything_written_is_playable() {
    let mut rng = Rng::new(0x0071_2111);
    let (mut accepted, mut rejected) = (0, 0);
    for _ in 0..20_000 {
        let text = random_text(&mut rng);
        let signature = signature(&mut rng);
        let key = Scale::parse(["A minor", "C", "F# dorian", "Bb major pentatonic"][rng.below(4)]).unwrap();

        match Chord::parse(&text) {
            Ok(chord) => {
                accepted += 1;
                // The canonical name is itself a chord symbol for the same chord.
                let again = Chord::parse(&chord.name())
                    .unwrap_or_else(|error| panic!("{text:?} named {}: {error}", chord.name()));
                assert_eq!(again, chord, "{text:?} renamed {}", chord.name());
                assert_eq!(again.name(), chord.name());
                assert!(!chord.tone_names().is_empty());
                let voicing = voice(&chord, &spec(&mut rng), None);
                let next = voice(&chord, &spec(&mut rng), Some(&voicing));
                assert!(next.pitches().iter().all(|pitch| *pitch <= 127));
                let _ = identify(&voicing.pitches());
            }
            Err(error) => {
                rejected += 1;
                assert_explains(&error, &text);
            }
        }
        if let Err(error) = Scale::parse(&text) {
            assert_explains(&error, &text);
        }
        if let Err(error) = roman::realize(&text, &key) {
            assert_explains(&error, &text);
        }
        if let Err(error) = StepPattern::parse(&text).map(|pattern| pattern.hits_for(signature.beats_per_bar(), 16.0)) {
            assert_explains(&error, &text);
        }
        match DrumPattern::parse([("kick", text.as_str()), ("hat", "x.x.")]) {
            Ok(pattern) => {
                assert_playable(&pattern.notes(3, signature.beats_per_bar(), 1.0), &text);
                let _ = pattern.odd_lanes(signature.beats_per_bar());
            }
            Err(error) => assert_explains(&error, &text),
        }
        match Phrase::parse(&text, Some(&key), rng.below(12) as i32 - 2, signature) {
            Ok(phrase) => assert_playable(&phrase.notes(0.9, &Default::default(), signature), &text),
            Err(error) => assert_explains(&error, &text),
        }
        let rhythm = Rhythm::parse(&text, signature).unwrap_or(Rhythm::Sustain);
        let _ = (
            parse_beats(&text),
            note_value(&text),
            VoicingStyle::parse(&text),
            ArpOrder::parse(&text),
            BassStyle::parse(&text),
        );

        match Progression::parse(&text, signature, Some(&key)) {
            Ok(progression) => {
                let progression = progression.cycled(1 + rng.below(6));
                let voicings = progression.voice(&spec(&mut rng));
                let comping =
                    Comping { rhythm, anticipation: rng.unit(), strum: rng.unit() * 0.1, ..Comping::default() };
                let mut notes = comp(&progression, &voicings, &comping);
                let order = [
                    ArpOrder::Up,
                    ArpOrder::UpDown,
                    ArpOrder::Random,
                    ArpOrder::Played,
                    ArpOrder::Pattern(vec![0, 1, 9, 3]),
                ];
                let arpeggio = Arpeggio {
                    order: order[rng.below(order.len())].clone(),
                    octaves: rng.below(8) as u8,
                    rate: 0.125 + rng.unit(),
                    ..Arpeggio::default()
                };
                notes.extend(arpeggio.notes(&progression, &voicings));
                for style in BassStyle::ALL {
                    let line = BassLine {
                        style,
                        low: rng.below(100) as u8,
                        high: rng.below(128) as u8,
                        ..BassLine::default()
                    };
                    notes.extend(bass_line(&progression, &line));
                }
                assert_playable(&notes, &text);
                let _ = progression.bar_names();
                let _ = estimate_key(&notes);
                let _ = summarize(&notes);
                let _ = read_chords(&notes, signature.beats_per_bar() / 2.0, 0.0, progression.length(), Some(&key));
            }
            Err(error) => assert_explains(&error, &text),
        }
    }
    // The mix of inputs really does exercise both outcomes.
    assert!(accepted > 500 && rejected > 5_000, "{accepted} accepted, {rejected} rejected");
}

#[test]
fn every_scale_and_degree_stays_on_the_keyboard() {
    let mut rng = Rng::new(42);
    for tonic in ["C", "C#", "Db", "D", "Eb", "E", "F", "F#", "Gb", "G", "Ab", "A", "Bb", "B", "Cb", "E#"] {
        for kind in trill_theory::ScaleKind::ALL {
            let scale = Scale::parse(&format!("{tonic} {}", kind.name())).unwrap();
            assert_eq!(Scale::parse(&scale.name()).unwrap(), scale, "{}", scale.name());
            for _ in 0..50 {
                let (degree, alter, octave) =
                    (rng.below(60) as i32 - 20, rng.below(5) as i32 - 2, rng.below(14) as i32 - 2);
                if let Some(pitch) = scale.degree_pitch(degree, alter, octave) {
                    assert!(pitch <= 127);
                }
            }
            for pitch in 0..=127u8 {
                for toward in [trill_theory::Snap::Nearest, trill_theory::Snap::Up, trill_theory::Snap::Down] {
                    assert!(scale.contains(scale.snap(pitch, toward)), "{} {pitch}", scale.name());
                }
                assert!(!scale.spell(pitch).is_empty());
            }
        }
    }
}
