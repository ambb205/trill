//! The wavetable synth's bank, held to what can be measured: every sound
//! through its own effects, as a track that is given it plays it.
//!
//! Nobody who wrote these sounds can hear them. So each is held to a
//! level, to the room a chord needs, to the middle for a bass, to the
//! time it may take, and to where the sounds of its kind lie in a synth
//! that is known to be good ([`REFERENCE`]: Serum 2's factory presets,
//! measured by the same rule on the same notes; `trill-mcp/tests/bank.rs`
//! measures them again on a machine that has them). What lies outside is
//! named here with why ([`OUTSIDE`]), and a name that is no longer needed
//! there fails the test.
//!
//! `cargo test -p trill-engine --test bank measured -- --ignored --nocapture`
//! prints every number.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Instant;
use trill_core::device::{DeviceFactory, Preset};
use trill_core::presets::PresetRef;
use trill_core::{ChainRef, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::character::{Character, Onset, Playing, character, onset, playing};
use trill_engine::render::{RenderOptions, render};

const SYNTH: &str = "builtin:wavetable";
const RATE: u32 = 48_000;

/// What a sound is matched to, in dBFS over its loudest 400 ms on the
/// phrase of its kind at a velocity of 100, through its effects. A sound
/// that holds is where the poly synth's presets lie by the same measure;
/// one that is struck and dies away has less to show over 400 ms for the
/// same peak, so its kind is matched lower, to where most of the kind has
/// the room to be.
fn level_of(kind: &str) -> f32 {
    match kind {
        "Pluck" | "Bell" => -20.0,
        "Keys" | "Strings" | "Brass" => -18.0,
        _ => -16.5,
    }
}
/// The room it leaves over that phrase, and over as full a chord as its
/// kind is given at full velocity: a sound that would have less at that
/// level is turned down until it has it.
const ROOM: f32 = -4.0;
const CHORD_ROOM: f32 = -0.5;
/// How near its volume is to the one those come to.
const TOLERANCE: f32 = 1.0;
/// What differs between the sides of a bass under 120 Hz, against what
/// they share, at the most.
const BASS_MIDDLE: f32 = -24.0;

/// A song that plays `notes` on the sound `preset`, with its effects
/// behind it and nothing on the master.
fn song(preset: &Preset, tempo: f64, notes: &[Note], beats: f64) -> Project {
    let mut project = Project::new(&preset.name);
    project.master.devices.clear();
    project.set_tempo(tempo).unwrap();
    let track = project.add_track(&preset.name);
    let device = project.add_device(ChainRef::Track(track), SYNTH, "Wavetable", None).unwrap();
    let model = project.device_mut(device).unwrap();
    model.params = preset.params.iter().cloned().collect();
    model.state = preset.state.clone();
    let sound = PresetRef { id: preset.name.clone(), name: preset.name.clone() };
    project.bring_after(device, &sound, preset.after.as_deref().unwrap_or_default()).unwrap();
    let clip = project.add_midi_clip(track, "Played", 0.0, beats).unwrap();
    project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
    project
}

fn rendered(project: &Project, tail: f64) -> (Vec<f32>, Vec<f32>) {
    let options = RenderOptions { sample_rate: RATE, max_tail_seconds: tail, ..RenderOptions::default() };
    let out = render(project, &BuiltinFactory, &options).unwrap_or_else(|error| panic!("{}: {error}", project.name));
    assert!(out.problems.is_empty(), "{}: {:?}", project.name, out.problems);
    (out.left, out.right)
}

/// Everything measured of one sound.
#[derive(Clone, Debug)]
struct Measured {
    name: String,
    kind: String,
    volume: f64,
    /// On the phrase of its kind.
    phrase: Character,
    /// The highest sample of its fullest chord at full velocity, in dBFS.
    chord_peak_db: f32,
    /// How one note begins and ends.
    note: Onset,
    /// The time its fullest chord took to render against the time it
    /// lasts.
    cost: f64,
}

impl Measured {
    /// What its volume would have to be turned by to be at the level, or
    /// as near it as leaves the room.
    fn wanted(&self) -> f32 {
        let level = level_of(&self.kind);
        (level - self.phrase.level_db).min(ROOM - self.phrase.peak_db).min(CHORD_ROOM - self.chord_peak_db)
    }
}

fn presets() -> Vec<Preset> {
    let devices = BuiltinFactory.descriptors();
    let synth = devices.into_iter().find(|device| device.uri == SYNTH).expect("the wavetable synth");
    synth.presets.into_iter().filter(|preset| preset.name != "init").collect()
}

fn measure(preset: &Preset) -> Measured {
    let value = |id: &str| preset.params.iter().find(|(set, _)| set == id).map(|(_, value)| *value);
    let one_at_a_time = value("mode").is_some_and(|mode| mode != 0.0);
    let Playing { tempo, notes, beats, chord, single, held } = playing(&preset.category, one_at_a_time);
    let (left, right) = rendered(&song(preset, tempo, &notes, beats), 6.0);
    let phrase = character(&left, &right, f64::from(RATE));

    // As full a chord as the kind is given, as hard as it can be struck.
    let struck: Vec<Note> =
        chord.iter().map(|pitch| Note { pitch: *pitch, start: 0.0, length: 4.0, velocity: 127 }).collect();
    let project = song(preset, tempo, &struck, 4.0);
    let begun = Instant::now();
    let (left, right) = rendered(&project, 2.0);
    let took = begun.elapsed().as_secs_f64();
    let cost = took / (left.len() as f64 / f64::from(RATE));
    let chord_peak_db = character(&left, &right, f64::from(RATE)).peak_db;

    let one = [Note { pitch: single, start: 0.0, length: held, velocity: 100 }];
    let (left, right) = rendered(&song(preset, tempo, &one, held), 8.0);
    let off = (held * 60.0 / tempo * f64::from(RATE)) as usize;
    let note = onset(&left, &right, f64::from(RATE), 0, off.min(left.len()));
    Measured {
        name: preset.name.clone(),
        kind: preset.category.clone(),
        volume: value("volume").unwrap_or(-21.0),
        phrase,
        chord_peak_db,
        note,
        cost,
    }
}

/// The bank, measured once for all the tests of this file.
fn bank() -> &'static [Measured] {
    static BANK: OnceLock<Vec<Measured>> = OnceLock::new();
    BANK.get_or_init(|| presets().iter().map(measure).collect())
}

/// What the sounds of a kind measure in the synth they are held against:
/// the least and the most of each number among a spread of its presets of
/// that kind.
struct Reference {
    kind: &'static str,
    centre_hz: (f32, f32),
    low_db: (f32, f32),
    high_db: (f32, f32),
    width_db: (f32, f32),
    attack_s: (f32, f32),
    release_s: (f32, f32),
}

/// Serum 2's factory presets by kind, as `trill-mcp/tests/bank.rs`
/// measured them on this rule (the date and the count are with the
/// numbers there). Measurements only: no sound of theirs is rebuilt here,
/// and nothing of them is kept but these ranges.
const REFERENCE: &[Reference] = &[];

/// The sounds that lie outside what their kind measures there, each with
/// which number and why it is meant to.
const OUTSIDE: &[(&str, &str, &str)] = &[];

fn report(bank: &[Measured]) -> String {
    let mut said = String::from(
        "sound                kind      level    peak   chord   to match | centre   <250   >4k   width  low width | attack release | cost\n",
    );
    for sound in bank {
        let Measured { phrase, note, .. } = sound;
        said.push_str(&format!(
            "{:<20} {:<8} {:>6.1} {:>7.1} {:>7.1} {:>8.1}  | {:>6.0} {:>6.1} {:>6.1} {:>6.1} {:>9.1}  | {:>6.3} {:>6.2}  | {:>4.1} %\n",
            sound.name,
            sound.kind,
            phrase.level_db,
            phrase.peak_db,
            sound.chord_peak_db,
            sound.volume + f64::from(sound.wanted()),
            phrase.centre_hz,
            phrase.low_db,
            phrase.high_db,
            phrase.width_db,
            phrase.low_width_db,
            note.attack_s,
            note.release_s,
            sound.cost * 100.0,
        ));
    }
    said
}

/// Not a test: every number of every sound, and the volume each would
/// have to have to be at the level.
#[test]
#[ignore = "a table: cargo test -p trill-engine --test bank measured -- --ignored --nocapture"]
fn the_bank_measured() {
    println!("{}", report(bank()));
    // The spread of each kind, as the reference's is given.
    let mut kinds: BTreeMap<&str, Vec<&Measured>> = BTreeMap::new();
    for sound in bank() {
        kinds.entry(sound.kind.as_str()).or_default().push(sound);
    }
    let span = |of: &[&Measured], number: &dyn Fn(&Measured) -> f32| {
        let values: Vec<f32> = of.iter().map(|sound| number(sound)).collect();
        let (least, most) = values.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        format!("{least:.2} to {most:.2}")
    };
    for (kind, sounds) in kinds {
        println!(
            "{kind} ({}): centre {} Hz, under 250 Hz {} dB, over 4 kHz {} dB, width {} dB, attack {} s, release {} s",
            sounds.len(),
            span(&sounds, &|s| s.phrase.centre_hz),
            span(&sounds, &|s| s.phrase.low_db),
            span(&sounds, &|s| s.phrase.high_db),
            span(&sounds, &|s| s.phrase.width_db),
            span(&sounds, &|s| s.note.attack_s),
            span(&sounds, &|s| s.note.release_s),
        );
    }
}

#[test]
fn every_sound_is_at_one_level_through_its_effects_and_leaves_room_for_a_chord() {
    let mut wrong = Vec::new();
    for sound in bank() {
        // Never over full scale, however hard and however full.
        assert!(sound.chord_peak_db < 0.0, "{} goes over full scale: {:.1} dBFS", sound.name, sound.chord_peak_db);
        assert!(sound.phrase.level_db > -40.0, "{} is all but silent", sound.name);
        let wanted = sound.wanted();
        if wanted.abs() > TOLERANCE {
            wrong.push(format!(
                "{} ({}) is at {:.1} dBFS, peak {:.1}, its chord at {:.1}: its volume wants {wanted:+.1} dB, {:.1}",
                sound.name,
                sound.kind,
                sound.phrase.level_db,
                sound.phrase.peak_db,
                sound.chord_peak_db,
                sound.volume + f64::from(wanted)
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // Within a kind the same playing is the same loudness: most of the
    // kind is at its level, and the few that have not the room for it (a
    // sound that is over before 400 ms are) are named, and no further
    // under it than they were found to be.
    let mut short: Vec<(&str, f32)> = Vec::new();
    for kind in trill_engine::character::KINDS {
        let of_kind: Vec<&Measured> = bank().iter().filter(|sound| sound.kind == *kind).collect();
        let under: Vec<&&Measured> =
            of_kind.iter().filter(|sound| sound.phrase.level_db < level_of(kind) - TOLERANCE).collect();
        assert!(2 * under.len() <= of_kind.len(), "{kind}: {} of {} are under its level", under.len(), of_kind.len());
        assert!(of_kind.iter().all(|sound| sound.phrase.level_db <= level_of(kind) + TOLERANCE), "{kind}");
        short.extend(under.iter().map(|sound| (sound.name.as_str(), level_of(kind) - sound.phrase.level_db)));
    }
    let named: Vec<&str> = SHORT_OF_ITS_LEVEL.iter().map(|(name, _)| *name).collect();
    for (name, by) in &short {
        let allowed = SHORT_OF_ITS_LEVEL.iter().find(|(known, _)| known == name).map(|(_, by)| *by);
        assert!(allowed.is_some_and(|allowed| *by <= allowed + 0.5), "{name} is {by:.1} dB under its kind's level");
    }
    for name in named {
        assert!(short.iter().any(|(short, _)| *short == name), "{name} is at its kind's level now");
    }
}

/// The sounds that are under their kind's level because a full chord of
/// them at full velocity, or the peaks of their own phrase, would
/// otherwise have no room: each is short, with most of a note in its first
/// tenth of a second. How many dB under each was found to be.
const SHORT_OF_ITS_LEVEL: &[(&str, f32)] = &[
    // A thump under a kick, ducked on every beat by its own chain.
    ("Rumble", 2.4),
    ("Tine Piano", 1.6),
    ("Clav", 3.7),
    ("Dusty Keys", 1.7),
    ("Music Box", 1.5),
    ("Marimba", 2.7),
    ("Staccato Strings", 2.7),
    ("Brass Stab", 3.2),
    // Chopped every sixteenth: its peaks stand over what 400 ms hold.
    ("Gated Pad", 1.1),
    // Thirty-two voices beating: the same.
    ("Swarm", 1.4),
    ("Laser", 3.1),
];

#[test]
fn a_bass_is_in_the_middle_under_120_hz() {
    for sound in bank().iter().filter(|sound| sound.kind == "Bass") {
        assert!(
            sound.phrase.low_width_db <= BASS_MIDDLE,
            "{}: under 120 Hz its sides differ by {:.1} dB against what they share",
            sound.name,
            sound.phrase.low_width_db
        );
        // And it is a bass: most of it lies low.
        assert!(sound.phrase.centre_hz < 700.0, "{}: its weight lies at {:.0} Hz", sound.name, sound.phrase.centre_hz);
    }
}

#[test]
fn every_sound_lies_where_its_kind_lies_in_a_synth_known_to_be_good_or_says_why_not() {
    let mut outside: Vec<String> = Vec::new();
    let mut excused: Vec<(&str, &str)> = Vec::new();
    for sound in bank() {
        let Some(kind) = REFERENCE.iter().find(|kind| kind.kind == sound.kind) else { continue };
        let numbers: [(&str, f32, (f32, f32)); 6] = [
            ("centre", sound.phrase.centre_hz, kind.centre_hz),
            ("low", sound.phrase.low_db, kind.low_db),
            ("high", sound.phrase.high_db, kind.high_db),
            ("width", sound.phrase.width_db, kind.width_db),
            ("attack", sound.note.attack_s, kind.attack_s),
            ("release", sound.note.release_s, kind.release_s),
        ];
        for (number, value, (least, most)) in numbers {
            if (least..=most).contains(&value) {
                continue;
            }
            match OUTSIDE.iter().find(|(name, which, _)| *name == sound.name && *which == number) {
                Some((name, which, _)) => excused.push((name, which)),
                None => outside.push(format!(
                    "{} ({}): {number} is {value:.2}, and its kind lies between {least:.2} and {most:.2}",
                    sound.name, sound.kind
                )),
            }
        }
    }
    assert!(outside.is_empty(), "{outside:#?}");
    // A reason that is no longer needed is not kept.
    for (name, which, why) in OUTSIDE {
        assert!(!why.is_empty(), "{name}: {which} lies outside without a word of why");
        assert!(excused.contains(&(*name, *which)), "{name}: {which} no longer lies outside its kind");
    }
}

#[test]
fn every_sound_plays_its_fullest_chord_in_far_less_than_the_time_it_lasts() {
    // On a machine that is doing nothing else a chord of the costliest
    // sound takes a twentieth of the time it lasts, effects included; a
    // third is what a busy one leaves.
    for sound in bank() {
        assert!(sound.cost < 0.33, "{}: {:.0} % of the time its chord lasts", sound.name, sound.cost * 100.0);
    }
}
