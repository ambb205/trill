//! Throws random and near-valid text at every notation the crate reads.
//! Whatever comes in, the answer is a value or an error with a message:
//! never a panic, and never a note the project model would reject.

use trill_core::{Articulation, InstrumentProfile, Lasts, Note, Role, TimeSignature};
use trill_theory::arpeggio::{ArpOrder, Arpeggio};
use trill_theory::articulation::articulate;
use trill_theory::bass::{BassLine, BassStyle, bass_line};
use trill_theory::drums::{DrumPattern, FillPlan, arrange};
use trill_theory::melody::Phrase;
use trill_theory::ornament::{Playing, perform};
use trill_theory::progression::{Comping, Progression, Rhythm, comp};
use trill_theory::roman;
use trill_theory::timing::{note_value, parse_beats};
use trill_theory::voicing::{Bass, VoicingSpec, VoicingStyle, voice};
use trill_theory::{Chord, Rng, Scale, StepPattern, estimate_key, identify, read_chords, summarize};

/// Characters the notations use, plus some they do not.
const ALPHABET: &[&str] = &[
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "a",
    "b",
    "c",
    "d",
    "g",
    "i",
    "j",
    "m",
    "M",
    "n",
    "o",
    "O",
    "r",
    "s",
    "t",
    "u",
    "v",
    "x",
    "X",
    "I",
    "V",
    "#",
    "b",
    "♭",
    "♯",
    "°",
    "ø",
    "Δ",
    "^",
    "+",
    "-",
    "/",
    "(",
    ")",
    ",",
    ".",
    ":",
    "|",
    "%",
    "~",
    "_",
    "'",
    " ",
    " ",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "9",
    "11",
    "13",
    "maj",
    "min",
    "dim",
    "aug",
    "sus",
    "add",
    "no",
    "alt",
    "N.C.",
    "é",
    "🎹",
    "\t",
    "\n",
    "",
    "!",
    "=",
    "!bend=",
    "!vib",
    "!slide",
    "!h",
    "!p",
    "!palm_mute",
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
    ":0.5 E4 F4!hammer E4!pull r G4!slide A4!slide=C5 | E4:4!bend=F#4!vib",
    "1!bend=3 ~ 1!release 5!prebend=6!vib=0.5 5:2!fall",
    "E3!palm_mute E3!h G3!p A3!legato B3!slide!vib",
    "5!bend=5' 5!slide=1,, 1!vib=9 1!prebend=b2!release",
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

/// Instruments as a track may know them: nothing known, a guitar with
/// keyswitches under its range, a wind instrument that slurs, and one whose
/// wheel bends an unusual way.
fn instrument(rng: &mut Rng) -> InstrumentProfile {
    match rng.below(4) {
        0 => InstrumentProfile::default(),
        1 => InstrumentProfile {
            articulations: vec![
                Articulation::key("open", 24).hit(64, 126).as_default(),
                Articulation::key("muted", 24).hit(32, 63),
                Articulation::key("palm_mute", 26),
                Articulation::key("slide", 27).lasting(Lasts::Once),
                Articulation::key("glide", 28).lasting(Lasts::Joins),
                Articulation::key("hammer", 29).lasting(Lasts::Joins),
            ],
            range: Some((40, 88)),
            sounds_octaves: [0, -1, 1][rng.below(3)],
            ..InstrumentProfile::default()
        },
        2 => InstrumentProfile { slurs: true, range: Some((49, 81)), ..InstrumentProfile::default() },
        _ => InstrumentProfile { bend_range: Some(0.25 + rng.unit() * 47.0), ..InstrumentProfile::default() },
    }
}

/// A drum kit that keeps some drums on keys of its own.
fn kit(rng: &mut Rng) -> InstrumentProfile {
    let moved = [("closed_hat_edge", 22u8), ("china", 86), ("kick", 24), ("snare", 127), ("high_tom", 0)];
    let sounds = moved.iter().filter(|_| rng.below(2) == 0).map(|(name, key)| ((*name).to_string(), *key)).collect();
    InstrumentProfile { role: Some(Role::Drums), sounds, ..InstrumentProfile::default() }
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
                // On a kit with drums of its own, by any name, and with a fill over its end.
                let kit = kit(&mut rng);
                let mut on_kit = pattern.on_kit(&kit);
                match on_kit.set_lane_on(&kit, &text, "x...x...") {
                    Ok(_) => {}
                    Err(error) => assert_explains(&error, &text),
                }
                let _ = on_kit.set_lane_on(&kit, ["china", "closed_hat_edge", "ride_bell"][rng.below(3)], &text);
                let fill = FillPlan { pattern: &pattern, from: rng.unit(), every: rng.below(5), crash: 49 };
                let arranged = arrange(&on_kit, Some(&fill), rng.below(9), signature.beats_per_bar(), 0.1 + rng.unit());
                assert_playable(&arranged.notes, &text);
                let _ = on_kit.grid();
            }
            Err(error) => assert_explains(&error, &text),
        }
        match Phrase::parse(&text, Some(&key), rng.below(12) as i32 - 2, signature) {
            Ok(phrase) => {
                assert_playable(&phrase.notes(0.9, &Default::default(), signature), &text);
                let _ = play(&phrase, &text, signature, &mut rng);
            }
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
    assert!(accepted > 300 && rejected > 5_000, "{accepted} accepted, {rejected} rejected");
}

/// Lines put together from the parts of the notation, most of them with
/// ornaments and articulations, played on every kind of instrument.
#[test]
fn ornamented_lines_are_playable_on_whatever_instrument_they_are_written_for() {
    const TONES: &[&str] = &["1", "5", "b3", "#4", "5'", "1,", "3", "E4", "F#4", "Bb2", "C6", "A3", "r"];
    const LENGTHS: &[&str] = &["", "", "", ":0.5", ":2", ":4", ":0.33"];
    const ORNAMENTS: &[&str] = &[
        "!bend=5",
        "!bend=F#4",
        "!release",
        "!prebend=6",
        "!slide",
        "!slide=1",
        "!hammer",
        "!pull",
        "!legato",
        "!h",
        "!p",
        "!vib",
        "!vib=0.5",
        "!fall",
    ];
    // Ways of playing that only some instruments have, and marks that are none.
    const NAMED: &[&str] = &["!palm_mute", "!muted", "!open"];
    const BROKEN: &[&str] = &["!nothing", "!", "!bend=", "!vib=40", "!slide=C8", "!bend=1,,,,", ":0.001", "~", "G9"];
    let mut rng = Rng::new(0x0bad_5eed);
    let (mut performed, mut bent, mut refused) = (0, 0, 0);
    for _ in 0..6_000 {
        let mut text = String::from([":0.5 ", ":1 ", "", ":0.25 "][rng.below(4)]);
        for _ in 0..1 + rng.below(10) {
            let tone = TONES[rng.below(TONES.len())];
            text.push_str(tone);
            text.push_str(LENGTHS[rng.below(LENGTHS.len())]);
            // Mostly as the notation allows: a rest is rarely marked, a bar line rarely drawn.
            let marked = if tone == "r" { 39 - rng.below(2) * 20 } else { rng.below(40) };
            match marked {
                0..=15 => text.push_str(ORNAMENTS[rng.below(ORNAMENTS.len())]),
                16..=19 => {
                    text.push_str(ORNAMENTS[rng.below(ORNAMENTS.len())]);
                    text.push_str(ORNAMENTS[rng.below(ORNAMENTS.len())]);
                }
                20..=22 => text.push_str(NAMED[rng.below(NAMED.len())]),
                23 => text.push_str(BROKEN[rng.below(BROKEN.len())]),
                _ => {}
            }
            text.push_str(if rng.below(30) == 0 { " | " } else { " " });
        }
        let signature = signature(&mut rng);
        let key = Scale::parse(["A minor", "C", "F# dorian", "Bb major pentatonic"][rng.below(4)]).unwrap();
        match Phrase::parse(&text, Some(&key), rng.below(12) as i32 - 2, signature) {
            Ok(phrase) => match play(&phrase, &text, signature, &mut rng) {
                Some(wheel) => {
                    performed += 1;
                    bent += usize::from(wheel);
                }
                None => refused += 1,
            },
            Err(error) => {
                refused += 1;
                assert_explains(&error, &text);
            }
        }
    }
    // Many are refused, as they should be: a release without a bend, a hammer-on with no note before it.
    assert!(performed > 500 && bent > 300 && refused > 500, "{performed} performed, {bent} bent, {refused} refused");
}

/// Plays `phrase` as an instrument does: its ornaments as that
/// instrument's keys or as the pitch wheel, then its keyswitches. Whether
/// the wheel moved, or `None` for a line the instrument cannot play as
/// asked, which has to say why.
fn play(phrase: &Phrase, text: &str, signature: TimeSignature, rng: &mut Rng) -> Option<bool> {
    let profile = instrument(rng);
    let playing = Playing {
        gate: rng.unit() * 1.2,
        dynamics: Default::default(),
        signature,
        bars: &[],
        tempo: 40.0 + rng.unit() * 200.0,
        profile: &profile,
        articulation: [None, None, Some("palm_mute"), Some("nothing it has")][rng.below(4)],
    };
    let performance = match perform(&phrase.tones, &playing) {
        Ok(performance) => performance,
        Err(error) => {
            assert_explains(&error, text);
            return None;
        }
    };
    let notes: Vec<Note> = performance.notes.iter().map(|(note, _)| *note).collect();
    assert_playable(&notes, text);
    for point in &performance.wheel {
        assert!(point.beat.is_finite() && point.beat >= 0.0, "{text:?}: wheel at {}", point.beat);
        assert!((-1.0..=1.0).contains(&point.value), "{text:?}: wheel value {}", point.value);
    }
    assert!(performance.wheel.windows(2).all(|pair| pair[0].beat <= pair[1].beat), "{text:?}: the wheel goes back");
    // The part lies `room` beats into its clip: a keyswitch may be pressed in that room, before
    // the part's own beat 0, and nowhere earlier.
    let room = [0.0, 0.01, rng.unit()][rng.below(3)];
    match articulate(&profile, &performance.notes, room) {
        Ok(articulated) => {
            let in_clip: Vec<Note> =
                articulated.notes.iter().map(|note| Note { start: note.start + room, ..*note }).collect();
            assert_playable(&in_clip, text);
            Some(!performance.wheel.is_empty())
        }
        Err(error) => {
            assert_explains(&error, text);
            None
        }
    }
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
