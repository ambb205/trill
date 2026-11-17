//! What the sampled instrument does with notes, on instruments made up in
//! memory: recordings that are a steady level say which of them sounds,
//! and tones say at what pitch and how smoothly.

use super::*;
use crate::testing::*;
use std::time::{Duration, Instant};
use trill_core::device::{DeviceProcessor, Event, EventKind, ProcessInfo};

const SR: f64 = 48_000.0;
const FILE: &str = "/i/Made Up.sfz";

/// A player of the instrument `sfz` describes, whose recordings are
/// `files` (mono, at 48 kHz, beside the instrument's file), at unity
/// volume. With it comes what its recordings are held in.
fn made(sfz: &str, files: &[(&str, Vec<f32>)], purpose: Purpose) -> (Player, Source) {
    let loader = MemoryLoader::default();
    for (name, samples) in files {
        loader.insert(&format!("/i/{name}"), samples.clone(), SR);
    }
    let texts = MemoryTexts::default();
    texts.insert(FILE, sfz);
    let source = Source::with_texts(Arc::new(loader), Box::new(texts));
    let loaded = source.load(Some(&State::new(FILE).to_bytes())).unwrap().unwrap();
    let mut player = Player::new(Some(loaded), Some(source.pool.clone()), purpose);
    player.prepare(SR, 512);
    set(&mut player, "volume", 0.0);
    (player, source)
}

/// The same, made to be rendered: it waits for what it plays.
fn rendered(sfz: &str, files: &[(&str, Vec<f32>)]) -> Player {
    made(sfz, files, Purpose::Rendering).0
}

fn set(player: &mut Player, id: &str, value: f64) {
    let index = PARAMS.iter().position(|def| def.id == id).unwrap_or_else(|| panic!("no parameter '{id}'"));
    player.set_param(index, value);
}

/// Plays `events` (frame, event) through `player` in blocks of `block`,
/// as a song at 120 does that began `from` frames ago.
fn play_from(
    player: &mut Player,
    from: usize,
    block: usize,
    frames: usize,
    events: &[(usize, EventKind)],
) -> (Vec<f32>, Vec<f32>) {
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut start = 0;
    while start < frames {
        let end = (start + block).min(frames);
        let info = ProcessInfo {
            sample_rate: SR,
            tempo: 120.0,
            time_signature: Default::default(),
            playing: true,
            position_beats: (from + start) as f64 / SR * 2.0,
            song: Default::default(),
            loop_range: None,
        }
        .steady();
        let now: Vec<Event> = events
            .iter()
            .filter(|(frame, _)| (start..end).contains(frame))
            .map(|(frame, kind)| Event { offset: (frame - start) as u32, kind: *kind })
            .collect();
        player.process(&info, &now, &mut left[start..end], &mut right[start..end]);
        start = end;
    }
    (left, right)
}

fn play(player: &mut Player, block: usize, frames: usize, events: &[(usize, EventKind)]) -> Vec<f32> {
    play_from(player, 0, block, frames, events).0
}

fn on(pitch: u8, velocity: u8) -> EventKind {
    EventKind::NoteOn { pitch, velocity: f32::from(velocity) / 127.0 }
}

fn off(pitch: u8) -> EventKind {
    EventKind::NoteOff { pitch }
}

fn control(controller: u8, value: f32) -> EventKind {
    EventKind::Control { controller, value }
}

fn level(frames: usize, level: f32) -> Vec<f32> {
    vec![level; frames]
}

fn cents(heard: f64, wanted: f64) -> f64 {
    1_200.0 * (heard / wanted).log2()
}

/// Every level is as recorded: the velocity decides nothing.
const FLAT: &str = "<global> amp_veltrack=0\n";

#[test]
fn the_root_key_plays_the_recording_as_it_is_and_every_other_key_at_its_pitch() {
    let recording: Vec<f32> = sine(440.0, SR, 96_000).iter().map(|sample| sample * 0.5).collect();
    let sfz = format!("{FLAT}<region> sample=a4.wav pitch_keycenter=69");
    let mut player = rendered(&sfz, &[("a4.wav", recording.clone())]);
    let heard = play(&mut player, 256, 100_000, &[(100, on(69, 100))]);
    assert_eq!(peak(&heard[..100]), 0.0);
    for (frame, wanted) in recording.iter().enumerate() {
        assert!((heard[100 + frame] - wanted).abs() < 1e-6, "frame {frame}");
    }
    assert_eq!(peak(&heard[96_100..]), 0.0, "silent once the recording has ended");

    // A semitone, a fifth, an octave and two either way: within a
    // hundredth of a cent of where the key is.
    for key in [45u8, 57, 62, 68, 70, 76, 81, 93] {
        let mut player = rendered(&sfz, &[("a4.wav", recording.clone())]);
        let frames = if key > 69 { 30_000 } else { 60_000 };
        let heard = play(&mut player, 256, frames, &[(0, on(key, 100))]);
        let wanted = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
        let off_by = cents(zero_crossing_hz(&heard[2_000..frames - 2_000], SR), wanted);
        assert!(off_by.abs() < 0.01, "key {key} is {off_by:.4} cents off");
    }

    // The instrument's own tuning, the device's, and a recording at another rate than the engine's.
    let tuned = format!("{FLAT}<region> sample=a4.wav pitch_keycenter=69 tune=50 transpose=-1 pitch_keytrack=50");
    let mut player = rendered(&tuned, &[("a4.wav", recording.clone())]);
    set(&mut player, "transpose", 2.0);
    set(&mut player, "tune", -25.0);
    let heard = play(&mut player, 256, 40_000, &[(0, on(73, 100))]);
    // Four keys up at half a semitone each, -50, +200, -25.
    let wanted = 440.0 * 2f64.powf((200.0 - 50.0 + 200.0 - 25.0) / 1_200.0);
    assert!(cents(zero_crossing_hz(&heard[2_000..38_000], SR), wanted).abs() < 0.01);
}

#[test]
fn the_keys_and_how_hard_they_are_struck_decide_which_recording_sounds() {
    let sfz = format!(
        "{FLAT}<group> lokey=36 hikey=59\n<region> sample=low_soft.wav lovel=1 hivel=64\n<region> sample=low_loud.wav lovel=65 hivel=127\n\
         <group> lokey=60 hikey=84 pitch_keytrack=0\n<region> sample=high.wav"
    );
    let files = [("low_soft.wav", level(9_000, 0.1)), ("low_loud.wav", level(9_000, 0.2)), ("high.wav", level(9_000, 0.4))];
    let heard = |key: u8, velocity: u8| {
        let mut player = rendered(&sfz, &files);
        play(&mut player, 256, 4_000, &[(0, on(key, velocity))])[2_000]
    };
    assert!((heard(48, 30) - 0.1).abs() < 1e-6 && (heard(48, 64) - 0.1).abs() < 1e-6);
    assert!((heard(59, 65) - 0.2).abs() < 1e-6 && (heard(36, 127) - 0.2).abs() < 1e-6);
    assert!((heard(60, 1) - 0.4).abs() < 1e-6 && (heard(84, 127) - 0.4).abs() < 1e-6);
    assert_eq!((heard(35, 100), heard(85, 100)), (0.0, 0.0), "outside every region nothing sounds");

    // The usual law, where the instrument says nothing: the square of the velocity; and the device's dynamics over it.
    let plain = "<region> sample=high.wav";
    let with = |dynamics: f64, velocity: u8| {
        let mut player = rendered(plain, &files);
        set(&mut player, "dynamics", dynamics);
        play(&mut player, 256, 4_000, &[(0, on(60, velocity))])[2_000] / 0.4
    };
    let half = (64.0f32 / 127.0).powi(2);
    assert!((with(1.0, 127) - 1.0).abs() < 1e-6 && (with(1.0, 64) - half).abs() < 1e-5);
    assert!((with(0.0, 64) - 1.0).abs() < 1e-6, "no dynamics: every note as loud as the hardest");
    assert!((with(2.0, 64) - half * half).abs() < 1e-5);
}

/// Three recordings of one note taken in turn, and four at random.
const TAKES: &str = "<global> amp_veltrack=0 pitch_keytrack=0 loop_mode=one_shot\n\
    <group> key=60 seq_length=3\n<region> sample=t1.wav seq_position=1\n<region> sample=t2.wav seq_position=2\n\
    <region> sample=t3.wav seq_position=3\n\
    <group> key=62\n<region> sample=t1.wav lorand=0 hirand=0.25\n<region> sample=t2.wav lorand=0.25 hirand=0.5\n\
    <region> sample=t3.wav lorand=0.5 hirand=0.75\n<region> sample=t4.wav lorand=0.75 hirand=1";

fn take_files() -> Vec<(&'static str, Vec<f32>)> {
    vec![("t1.wav", level(400, 0.1)), ("t2.wav", level(400, 0.2)), ("t3.wav", level(400, 0.3)), ("t4.wav", level(400, 0.4))]
}

#[test]
fn several_recordings_of_a_note_are_taken_in_turn_from_where_playing_began() {
    let mut player = rendered(TAKES, &take_files());
    let notes: Vec<(usize, EventKind)> = (0..7).map(|note| (note * 1_000, on(60, 100))).collect();
    let heard = play(&mut player, 256, 7_000, &notes);
    let takes: Vec<f32> = (0..7).map(|note| heard[note * 1_000 + 200]).collect();
    let wanted = [0.1, 0.2, 0.3, 0.1, 0.2, 0.3, 0.1];
    assert!(takes.iter().zip(wanted).all(|(heard, wanted)| (heard - wanted).abs() < 1e-6), "{takes:?}");
    // The seventh was the first take; after a reset the count begins again.
    player.reset();
    let heard = play(&mut player, 256, 2_000, &notes[..2]);
    assert!((heard[200] - 0.1).abs() < 1e-6 && (heard[1_200] - 0.2).abs() < 1e-6);
}

#[test]
fn a_take_at_random_is_a_matter_of_the_note_and_its_place_in_the_song() {
    // A note every 700 frames, at awkward places.
    let notes: Vec<(usize, EventKind)> = (0..64).map(|note| (note * 700 + (note * 37) % 211, on(62, 100))).collect();
    let song = |block: usize| {
        let mut player = rendered(TAKES, &take_files());
        play(&mut player, block, 46_000, &notes)
    };
    let heard = song(256);
    // Each note is one of the four takes, and all four are taken.
    let takes: Vec<u8> = notes.iter().map(|(frame, _)| (heard[frame + 100] * 10.0).round() as u8).collect();
    for take in 1..=4u8 {
        let count = takes.iter().filter(|heard| **heard == take).count();
        assert!((6..=28).contains(&count), "take {take} was taken {count} times of 64: {takes:?}");
    }
    assert!(takes.iter().all(|take| (1..=4).contains(take)));
    // The same song is the same samples, whatever the size of the buffers.
    for block in [1, 16, 64, 333, 4_096] {
        assert!(song(block) == heard, "buffers of {block} frames");
    }
    // A note has its take wherever playing began: bar 17 sounds as it does in a render of the whole song.
    let mut player = rendered(TAKES, &take_files());
    // (From a frame at which nothing of the first half still sounds.)
    let later: Vec<(usize, EventKind)> = notes[32..].iter().map(|(frame, kind)| (frame - 22_400, *kind)).collect();
    let (from_there, _) = play_from(&mut player, 22_400, 256, 23_600, &later);
    assert!(from_there[..] == heard[22_400..], "the second half, played on its own");
    // And a key played while the song stands still is not always the same take.
    let mut player = rendered(TAKES, &take_files());
    let mut standing = Vec::new();
    for _ in 0..24 {
        let info = ProcessInfo { playing: false, ..info(SR) };
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        player.process(&info, &[Event { offset: 0, kind: on(62, 100) }], &mut left, &mut right);
        standing.push((left[200] * 10.0).round() as u8);
    }
    standing.dedup();
    assert!(standing.len() > 4, "{standing:?}");
}

#[test]
fn a_held_note_goes_round_its_loop_without_a_click_and_on_to_the_end_when_the_instrument_says_so() {
    // The loop holds 30 whole waves, so going round it is the tone going on.
    let period = 120.0;
    let tone = sine(SR / period, SR, 12_000);
    let natural = (std::f64::consts::TAU / period) as f32;
    for key in [57u8, 60, 64, 72] {
        let sfz = format!("{FLAT}<region> sample=tone.wav loop_mode=loop_continuous loop_start=3000 loop_end=6599 ampeg_release=0.05");
        let mut player = rendered(&sfz, &[("tone.wav", tone.clone())]);
        let heard = play(&mut player, 256, 96_000, &[(0, on(key, 100)), (80_000, off(key))]);
        let rate = 2f32.powf((f32::from(key) - 60.0) / 12.0);
        assert!(largest_step(&heard[100..]) < natural * rate * 1.05, "key {key}: a step of {}", largest_step(&heard[100..]));
        assert!(rms(&heard[70_000..80_000]) > 0.69, "key {key}: the loop should sustain");
        let wanted = SR / period * f64::from(rate);
        assert!(cents(zero_crossing_hz(&heard[20_000..80_000], SR), wanted).abs() < 0.05, "key {key}");
        assert_eq!(peak(&heard[90_000..]), 0.0, "let go of, it ends");
    }

    // A loop only while the note is held: let go of, it plays on to the end of the recording.
    let staircase: Vec<f32> = (0..12_000).map(|frame| 0.1 + (frame / 3_000) as f32 * 0.1).collect();
    let sfz = format!("{FLAT}<region> sample=steps.wav loop_mode=loop_sustain loop_start=3000 loop_end=5999 ampeg_release=30");
    let mut player = rendered(&sfz, &[("steps.wav", staircase.clone())]);
    let heard = play(&mut player, 256, 40_000, &[(0, on(60, 100)), (20_000, off(60))]);
    assert!((heard[1_000] - 0.1).abs() < 1e-3 && (heard[4_000] - 0.2).abs() < 1e-3);
    assert!((heard[7_000] - 0.2).abs() < 1e-3 && (heard[19_000] - 0.2).abs() < 1e-3, "round the second step");
    // Let go of 2,000 frames into the loop: 7,000 more to the end.
    assert!((heard[22_000] - 0.3).abs() < 0.02 && (heard[26_000] - 0.4).abs() < 0.02, "on up the stairs");
    assert_eq!(peak(&heard[27_100..]), 0.0, "to the end of the recording");

    // A loop whose ends do not meet is faded across, where the instrument asks for that.
    let rough: Vec<f32> = sine(SR / 117.3, SR, 12_000);
    let joined = |crossfade: &str| {
        let sfz = format!("{FLAT}<region> sample=rough.wav loop_mode=loop_continuous loop_start=3000 loop_end=6599 {crossfade}");
        let mut player = rendered(&sfz, &[("rough.wav", rough.clone())]);
        largest_step(&play(&mut player, 256, 48_000, &[(0, on(60, 100))])[100..])
    };
    assert!(joined("") > 0.2, "the ends do not meet: {}", joined(""));
    assert!(joined("loop_crossfade=0.02") < 0.07, "faded across: {}", joined("loop_crossfade=0.02"));
}

#[test]
fn the_envelope_of_a_region_shapes_its_level() {
    let sfz = "<region> sample=dc.wav amp_veltrack=0 ampeg_delay=0.01 ampeg_attack=0.02 ampeg_hold=0.01 ampeg_decay=0.1 \
               ampeg_sustain=50 ampeg_release=0.2 ampeg_vel2attack=0.02";
    let mut player = rendered(sfz, &[("dc.wav", level(96_000, 0.8))]);
    let heard = play(&mut player, 256, 60_000, &[(0, on(60, 127)), (24_000, off(60))]);
    let at = |milliseconds: f32| heard[(milliseconds * 48.0) as usize] / 0.8;
    assert_eq!((at(0.0), at(9.9)), (0.0, 0.0), "the delay");
    // At full velocity the attack is 40 ms.
    assert!((at(30.0) - 0.5).abs() < 0.01 && (at(49.9) - 1.0).abs() < 0.01, "{} {}", at(30.0), at(49.9));
    assert!((at(59.0) - 1.0).abs() < 1e-3, "the hold");
    assert!(at(80.0) < 0.9 && at(80.0) > 0.6, "the decay: {}", at(80.0));
    assert!((at(400.0) - 0.5).abs() < 1e-3, "the sustain");
    // Sixty decibels down over the release.
    assert!((at(500.0 + 100.0) / 0.5 - 10f32.powf(-1.5)).abs() < 0.005, "{}", at(600.0));
    assert_eq!(at(900.0), 0.0);

    // An instrument that says nothing of its release still ends a note without a click.
    let tone = sine(100.0, SR, 96_000);
    let natural = (std::f64::consts::TAU * 100.0 / SR) as f32;
    let mut player = rendered(&format!("{FLAT}<region> sample=tone.wav"), &[("tone.wav", tone)]);
    let heard = play(&mut player, 256, 20_000, &[(1_000, on(60, 100)), (9_123, off(60))]);
    assert!(largest_step(&heard) < natural * 3.0, "a step of {} against {natural}", largest_step(&heard));
    assert!(rms(&heard[2_000..9_000]) > 0.6);
    assert_eq!(peak(&heard[11_000..]), 0.0, "and is over within forty milliseconds");
}

#[test]
fn the_pedal_holds_notes_and_a_key_coming_up_has_sounds_of_its_own() {
    let sfz = "<global> amp_veltrack=0 pitch_keytrack=0\n<region> sample=note.wav ampeg_release=0.01\n\
               <region> sample=thump.wav trigger=release rt_decay=6\n<region> sample=click.wav trigger=release_key key=61";
    let files = [("note.wav", level(96_000, 0.5)), ("thump.wav", level(2_400, 0.25)), ("click.wav", level(480, 0.125))];
    let pedal = |down: bool| control(64, if down { 1.0 } else { 0.0 });

    // Without the pedal: the note ends at its key, and the thump sounds there.
    let mut player = rendered(sfz, &files);
    let heard = play(&mut player, 256, 30_000, &[(0, on(60, 100)), (12_000, off(60))]);
    assert!((heard[6_000] - 0.5).abs() < 1e-6);
    // A quarter of a second held: 1.5 dB quieter.
    let wanted = 0.25 * 10f32.powf(-1.5 / 20.0);
    assert!((heard[13_500] - wanted).abs() < wanted * 0.01, "{} against {wanted}", heard[13_500]);
    assert_eq!(peak(&heard[14_500..]), 0.0);

    // With it: the note goes on past its key, and ends, with its thump, when the pedal comes up.
    let mut player = rendered(sfz, &files);
    let events = [(0, on(60, 100)), (2_400, pedal(true)), (4_800, off(60)), (24_000, pedal(false))];
    let heard = play(&mut player, 256, 36_000, &events);
    assert!((heard[4_900] - 0.5).abs() < 1e-6 && (heard[23_000] - 0.5).abs() < 1e-6, "held by the pedal, with no thump");
    // Half a second held by then: three decibels quieter.
    let wanted = 0.25 * 10f32.powf(-3.0 / 20.0);
    assert!((heard[25_500] - wanted).abs() < wanted * 0.01, "the thump when it comes up: {}", heard[25_500]);
    assert_eq!(peak(&heard[26_500..]), 0.0);

    // A sound for the key itself comes with the key, pedal or no.
    let mut player = rendered(sfz, &files);
    let events = [(0, pedal(true)), (0, on(61, 100)), (4_800, off(61)), (20_000, EventKind::AllNotesOff)];
    let heard = play(&mut player, 256, 30_000, &events);
    assert!((heard[4_900] - 0.625).abs() < 1e-6, "the note and its click: {}", heard[4_900]);
    assert!((heard[10_000] - 0.5).abs() < 1e-6);
    assert_eq!(peak(&heard[22_000..]), 0.0, "all notes off ends what the pedal holds, with no thump");

    // An instrument that does without the pedal.
    let mut player = rendered(&format!("{sfz}\n<group> sustain_sw=off <region> sample=note.wav key=90"), &files);
    let heard = play(&mut player, 256, 20_000, &[(0, pedal(true)), (0, on(60, 100)), (4_800, off(60))]);
    assert_eq!(peak(&heard[8_000..]), 0.0);
}

#[test]
fn layers_fade_into_one_another_by_how_hard_a_key_is_struck_and_by_a_controller() {
    // Two layers that cross between velocities 40 and 80.
    let by_velocity = format!(
        "{FLAT}<region> sample=soft.wav xfout_lovel=40 xfout_hivel=80\n<region> sample=loud.wav xfin_lovel=40 xfin_hivel=80"
    );
    let files = [("soft.wav", level(96_000, 0.2)), ("loud.wav", sine(1_000.0, SR, 96_000))];
    let parts = |velocity: u8| {
        let mut player = rendered(&by_velocity, &files);
        let heard = play(&mut player, 256, 9_600, &[(0, on(60, velocity))]);
        let soft = heard[4_800..9_600].iter().sum::<f32>() / 4_800.0 / 0.2;
        (soft, amplitude(&heard[4_800..9_600], 1_000.0, SR))
    };
    assert!((parts(20).0 - 1.0).abs() < 1e-3 && parts(20).1 < 1e-3, "{:?}", parts(20));
    assert!(parts(100).0.abs() < 1e-3 && (parts(100).1 - 1.0).abs() < 1e-3, "{:?}", parts(100));
    let (soft, loud) = parts(60);
    assert!((soft * soft + loud * loud - 1.0).abs() < 0.01, "half way they share the power: {soft} {loud}");

    // Two layers a controller fades between, while the note sounds.
    let by_wheel = format!(
        "{FLAT}<region> sample=soft.wav xfout_locc1=20 xfout_hicc1=100 xf_cccurve=gain\n\
         <region> sample=more.wav xfin_locc1=20 xfin_hicc1=100 xf_cccurve=gain"
    );
    let files = [("soft.wav", level(96_000, 0.2)), ("more.wav", level(96_000, 0.6))];
    let mut player = rendered(&by_wheel, &files);
    let mut events = vec![(0, on(60, 100))];
    // The wheel swept from bottom to top over a second, a value every 5 ms, and then let fall at once.
    events.extend((0..200).map(|step| (9_600 + step * 240, control(1, step as f32 / 199.0))));
    events.push((70_000, control(1, 0.0)));
    let heard = play(&mut player, 256, 80_000, &events);
    assert!((heard[9_000] - 0.2).abs() < 1e-6 && (heard[60_000] - 0.6).abs() < 1e-4, "{} {}", heard[9_000], heard[60_000]);
    let half_way = 9_600 + 100 * 240;
    // A little past the middle of the wheel's travel, which is a little past the middle of the crossing.
    assert!((heard[half_way + 400] - 0.42).abs() < 0.01, "{}", heard[half_way + 400]);
    assert!((heard[79_000] - 0.2).abs() < 1e-4, "back where it was");
    // Nowhere a step: the sweep, and the drop of a full 0.4, travel.
    assert!(largest_step(&heard[100..]) < 0.004, "a step of {}", largest_step(&heard[100..]));
    // The same whatever the buffers.
    for block in [32, 1_000] {
        let mut again = rendered(&by_wheel, &files);
        assert!(play(&mut again, block, 80_000, &events) == heard, "buffers of {block}");
    }
}

#[test]
fn a_key_that_switches_chooses_how_the_notes_after_it_are_played_and_sounds_nothing() {
    let sfz = format!(
        "<global> amp_veltrack=0 sw_lokey=24 sw_hikey=25 sw_default=24 lokey=36 hikey=96 pitch_keytrack=0\n\
         <region> sample=open.wav sw_last=24 sw_label=Open\n<region> sample=muted.wav sw_last=25 sw_label=Muted\n\
         <region> sample=slide.wav sw_down=25 lokey=97 hikey=97"
    );
    let files = [("open.wav", level(2_000, 0.1)), ("muted.wav", level(2_000, 0.2)), ("slide.wav", level(2_000, 0.4))];
    let mut player = rendered(&sfz, &files);
    let events = [
        (0, on(60, 100)),
        (4_000, on(25, 100)),
        (4_100, off(25)),
        (8_000, on(62, 100)),
        (12_000, on(24, 100)),
        (12_000, on(64, 100)),
        (16_000, on(97, 100)),
        (20_000, on(25, 100)),
        (20_010, on(97, 100)),
    ];
    let heard = play(&mut player, 256, 24_000, &events);
    let at = |frame: usize| heard[frame + 500];
    assert!((at(0) - 0.1).abs() < 1e-6, "the default before any switch is pressed");
    assert_eq!(at(4_000), 0.0, "the switch itself is silent");
    assert!((at(8_000) - 0.2).abs() < 1e-6 && (at(12_000) - 0.1).abs() < 1e-6);
    assert_eq!(at(16_000), 0.0, "a region for while a switch is held, and it is not");
    assert!((at(20_010) - 0.4).abs() < 1e-6, "and now it is");
}

#[test]
fn a_group_silences_another_and_a_key_has_only_so_many_voices() {
    // An open hat that the closed one cuts off, and a snare that rings over itself only twice.
    let sfz = "<global> amp_veltrack=0 loop_mode=one_shot\n<region> key=46 sample=open.wav group=1 off_by=2\n\
               <region> key=42 sample=closed.wav group=2\n<region> key=38 sample=snare.wav note_polyphony=2";
    let files = [("open.wav", level(48_000, 0.5)), ("closed.wav", level(2_400, 0.1)), ("snare.wav", level(48_000, 0.2))];
    let mut player = rendered(sfz, &files);
    let heard = play(&mut player, 256, 30_000, &[(0, on(46, 100)), (9_600, on(42, 100))]);
    assert!((heard[9_000] - 0.5).abs() < 1e-6);
    assert!((heard[9_600 + 400] - 0.1).abs() < 1e-6, "only the closed one, six milliseconds on: {}", heard[10_000]);
    assert!(largest_step(&heard[9_600..9_900]) < 0.12, "cut off quickly but not at once");
    assert_eq!(peak(&heard[12_100..]), 0.0);

    let mut player = rendered(sfz, &files);
    let hits: Vec<(usize, EventKind)> = (0..4).map(|hit| (hit * 4_800, on(38, 100))).collect();
    let heard = play(&mut player, 256, 24_000, &hits);
    assert!((heard[4_000] - 0.2).abs() < 1e-6 && (heard[9_000] - 0.4).abs() < 1e-6);
    assert!((heard[14_000] - 0.4).abs() < 1e-6 && (heard[19_000] - 0.4).abs() < 1e-6, "never three: {}", heard[14_000]);
}

#[test]
fn notes_over_the_limit_are_taken_without_a_click() {
    let tone = sine(100.0, SR, 96_000);
    let natural = (std::f64::consts::TAU * 100.0 / SR) as f32;
    let sfz = format!("{FLAT}<region> sample=tone.wav volume=-12 ampeg_release=0.3");
    let mut player = rendered(&sfz, &[("tone.wav", tone)]);
    set(&mut player, "voices", 2.0);
    // A new note every 50 ms, each held: every note takes a voice.
    let events: Vec<(usize, EventKind)> = (0..30).map(|note| (note * 2_400 + 77, on(55 + (note % 7) as u8, 100))).collect();
    let heard = play(&mut player, 256, 80_000, &events);
    // Up to three voices overlap while one goes, each at a quarter and up to a fifth above its recording.
    let limit = natural * 1.5 * 0.25 * 3.0 * 2.0;
    assert!(largest_step(&heard) < limit, "a step of {} (limit {limit})", largest_step(&heard));
    assert!(rms(&heard[40_000..]) > 0.1);
    // And two are all that sound: a level that sixteen voices of a quarter would pass.
    assert!(peak(&heard) < 0.8, "{}", peak(&heard));
}

#[test]
fn a_filter_a_vibrato_and_controllers_do_what_the_instrument_says() {
    let noise: Vec<f32> = (0..48_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 8) as f32 / 8_388_608.0 - 1.0).collect();
    let files = [("noise.wav", noise), ("tone.wav", sine(440.0, SR, 96_000))];
    let loud = |sfz: &str, events: &[(usize, EventKind)]| {
        let mut player = rendered(&format!("{FLAT}{sfz}"), &files);
        rms(&play(&mut player, 256, 24_000, events)[8_000..])
    };
    let note = [(0, on(60, 100))];
    let bright = loud("<region> sample=noise.wav", &note);
    let dark = loud("<region> sample=noise.wav cutoff=300 fil_type=lpf_2p", &note);
    assert!(dark < bright * 0.2, "{dark} against {bright}");
    // Its envelope opens it again, and so does a controller.
    let opened = loud("<region> sample=noise.wav cutoff=300 fileg_depth=7200 fileg_sustain=100", &note);
    assert!(opened > dark * 3.0, "{opened} against {dark}");
    let by_wheel = "<region> sample=noise.wav cutoff=300 cutoff_cc1=7200";
    assert!(loud(by_wheel, &[(0, control(1, 1.0)), (0, on(60, 100))]) > dark * 3.0);
    assert!((loud(by_wheel, &note) - dark).abs() < dark * 0.01);
    // A high-pass takes the bottom away instead.
    assert!(loud("<region> sample=tone.wav cutoff=4000 fil_type=hpf_2p", &note) < 0.05);

    // A controller turns the level, and the place between the sides.
    let half = loud("<region> sample=tone.wav gain_cc7=-6", &[(0, control(7, 1.0)), (0, on(60, 100))]);
    assert!((half / loud("<region> sample=tone.wav", &note) - 0.501).abs() < 0.01, "{half}");
    let mut player = rendered(&format!("{FLAT}<region> sample=tone.wav pan_oncc10=100"), &files);
    let (left, right) = play_from(&mut player, 0, 256, 9_600, &[(0, control(10, 1.0)), (0, on(60, 100))]);
    assert!(rms(&left[4_800..]) < 1e-4 && rms(&right[4_800..]) > 0.9);

    // Vibrato as deep as the wheel says: the pitch goes up and down by it, and not at all with the wheel down.
    let vibrato = "<region> sample=tone.wav pitchlfo_freq=5 pitchlfo_depth_oncc1=100";
    let swing = |wheel: f32| {
        let mut player = rendered(&format!("{FLAT}{vibrato}"), &files);
        let heard = play(&mut player, 256, 48_000, &[(0, control(1, wheel)), (0, on(69, 100))]);
        // The pitch over each fortieth of a second.
        let pitches: Vec<f64> = heard[4_800..].chunks(1_200).map(|chunk| cents(zero_crossing_hz(chunk, SR), 440.0)).collect();
        let (low, high) = pitches.iter().fold((f64::MAX, f64::MIN), |(low, high), cents| (low.min(*cents), high.max(*cents)));
        high - low
    };
    assert!(swing(0.0) < 3.0, "{}", swing(0.0));
    assert!((120.0..=210.0).contains(&swing(1.0)), "a swing of {} cents", swing(1.0));

    // The wheel bends as far as the instrument says, or as the device is set to.
    let bent = |sfz: &str, range: f64, wheel: f32| {
        let mut player = rendered(&format!("{FLAT}{sfz}"), &files);
        set(&mut player, "bend_range", range);
        let heard = play(&mut player, 256, 24_000, &[(0, EventKind::PitchBend { value: wheel }), (0, on(60, 100))]);
        cents(zero_crossing_hz(&heard[2_400..], SR), 440.0)
    };
    assert!((bent("<region> sample=tone.wav", 0.0, 1.0) - 200.0).abs() < 0.1, "{}", bent("<region> sample=tone.wav", 0.0, 1.0));
    assert!((bent("<region> sample=tone.wav bend_up=1200 bend_down=-700", 0.0, -1.0) + 700.0).abs() < 0.1);
    assert!((bent("<region> sample=tone.wav bend_up=1200", 5.0, 0.5) - 250.0).abs() < 0.1, "the device's own range");
}

#[test]
fn the_volume_pan_and_level_controllers_set_the_level_of_the_whole_instrument() {
    let files = [("dc.wav", level(48_000, 0.5))];
    let sfz = format!("{FLAT}<region> sample=dc.wav");
    let heard = |params: &[(&str, f64)], events: &[(usize, EventKind)]| {
        let mut player = rendered(&sfz, &files);
        for (id, value) in params {
            set(&mut player, id, *value);
        }
        let (left, right) = play_from(&mut player, 0, 256, 12_000, events);
        (left[9_000], right[9_000])
    };
    let note = [(0, on(60, 100))];
    let (left, right) = heard(&[], &note);
    assert!((left - 0.5).abs() < 1e-6 && left == right, "{left} {right}");
    let (left, right) = heard(&[("volume", -6.0), ("pan", 1.0)], &note);
    assert!(left.abs() < 1e-6 && (right - 0.5 * 0.501_187 * std::f32::consts::SQRT_2).abs() < 1e-4, "{left} {right}");
    assert_eq!(heard(&[("volume", -70.0)], &note), (0.0, 0.0));
    // Expression and channel volume, as on every instrument: the square of where they are.
    assert!((heard(&[], &[(0, control(11, 0.5)), (0, on(60, 100))]).0 - 0.125).abs() < 1e-5);
    assert!((heard(&[], &[(0, on(60, 100)), (2_000, control(7, 0.5))]).0 - 0.125).abs() < 1e-4);
    // A device made new plays at six decibels under the instrument.
    let mut player = rendered(&sfz, &files);
    set(&mut player, "volume", PARAMS[0].default);
    assert!((play(&mut player, 256, 4_000, &note)[2_000] - 0.5 * 0.501_187).abs() < 1e-4);
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let begun = Instant::now();
    while !done() {
        assert!(begun.elapsed() < Duration::from_secs(10), "{what} did not happen");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn played_live_a_note_begins_on_what_is_in_memory_and_goes_on_into_the_rest_when_that_is_read() {
    let recording: Vec<f32> = (0..120_000).map(|frame| 0.2 + 0.6 * (frame as f32 / 120_000.0)).collect();
    let sfz = format!("{FLAT}<region> sample=long.wav lokey=30\n<region> sample=short.wav key=20");
    let files = [("long.wav", recording.clone()), ("short.wav", level(4_000, 0.3))];

    // Rendered, it waits for what it needs: the recording from its first frame to its last.
    let whole = play(&mut rendered(&sfz, &files), 256, 121_000, &[(0, on(60, 100))]);
    assert!(whole[..120_000].iter().zip(&recording).all(|(heard, wanted)| (heard - wanted).abs() < 1e-6));

    // Live, with the starts read: the note begins at once, and a worker reads the rest behind it.
    let (mut player, source) = made(&sfz, &files, Purpose::Playing);
    let loaded = source.load(Some(&State::new(FILE).to_bytes())).unwrap().unwrap();
    wait_for("the starts being read", || loaded.settled() == (2, 2));
    assert!(loaded.recordings[0].whole_if_there().is_none(), "nothing is whole that was not played");
    let mut heard = play_from(&mut player, 0, 256, 2_048, &[(0, on(60, 100))]).0;
    wait_for("the rest being read", || loaded.recordings[0].whole_if_there().is_some());
    heard.extend(play_from(&mut player, 2_048, 256, 121_000 - 2_048, &[]).0);
    assert!(heard == whole, "the same samples as the render");
    assert_eq!(source.pool.held().short_notes, 0);

    // A second note finds it there; and one whose recording is short was whole all along.
    let again = play_from(&mut player, 0, 256, 121_000, &[(0, on(60, 100)), (0, on(20, 100))]).0;
    assert!((again[2_000] - (whole[2_000] + 0.3)).abs() < 1e-6, "{} against {}", again[2_000], whole[2_000] + 0.3);
    assert_eq!(again[100_000], whole[100_000]);
}

#[test]
fn a_note_whose_recording_is_not_there_in_time_is_faded_out_and_counted_and_one_not_read_at_all_is_silent() {
    let recording = sine(100.0, SR, 120_000);
    let natural = (std::f64::consts::TAU * 100.0 / SR) as f32;
    let sfz = format!("{FLAT}<region> sample=long.wav");
    let (mut player, source) = made(&sfz, &[("long.wav", recording)], Purpose::Playing);
    let loaded = source.load(Some(&State::new(FILE).to_bytes())).unwrap().unwrap();

    // Asked before the instrument's starts are read, a note makes no sound. (The workers are quick: the
    // note is given in the same breath as the device is made, and may find the start there after all.)
    let first = play(&mut player, 256, 512, &[(0, on(60, 100))]);
    let unready = source.pool.held().unready_notes;
    assert!(unready <= 1 && (unready == 1) == (peak(&first) == 0.0), "{unready} and a peak of {}", peak(&first));
    player.reset();

    wait_for("the start being read", || loaded.settled() == (1, 1));
    // Played through without a pause, far faster than any disk reads: the start runs out first.
    let heard = play(&mut player, 256, 40_000, &[(0, on(60, 100))]);
    if source.pool.held().short_notes == 1 {
        let last = heard.iter().rposition(|sample| sample.abs() > 1e-4).unwrap();
        assert!(last < START_FRAMES && last > START_FRAMES - 600, "it ended at frame {last}");
        assert!(largest_step(&heard) < natural * 1.5, "faded, not cut: a step of {}", largest_step(&heard));
        assert!(rms(&heard[..12_000]) > 0.6);
    } else {
        // A worker was quicker than the test, which it is allowed to be.
        assert!(rms(&heard[20_000..]) > 0.6);
    }
    // Whatever happened, the rest is read by now or will be, and the next note is whole.
    wait_for("the rest being read", || loaded.recordings[0].whole_if_there().is_some());
    player.reset();
    let heard = play(&mut player, 256, 40_000, &[(0, on(60, 100))]);
    assert!(rms(&heard[20_000..]) > 0.6);
}

#[test]
fn a_device_with_nothing_loaded_is_silent_and_one_whose_instrument_is_gone_says_so() {
    let mut empty = Player::new(None, None, Purpose::Playing);
    empty.prepare(SR, 512);
    let events = [(0, on(60, 100)), (500, off(60)), (600, EventKind::AllNotesOff), (700, control(64, 1.0))];
    assert_eq!(peak(&play(&mut empty, 256, 2_000, &events)), 0.0);

    let source = Source::with_texts(Arc::new(MemoryLoader::default()), Box::new(MemoryTexts::default()));
    let gone = State::new("/i/Gone.sfz").to_bytes();
    let (mut processor, mut controller) = make(Some(&gone), Some(&source), Purpose::Playing).unwrap();
    processor.prepare(SR, 512);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    processor.process(&info(SR), &[Event { offset: 0, kind: on(60, 100) }], &mut left, &mut right);
    assert_eq!(peak(&left), 0.0);
    assert_eq!(controller.save_state(), Some(gone.clone()), "its state is handed back as it was");
    assert!(controller.sampled().is_none() && controller.reading().is_none());
    let warnings = source.warnings(Some(&gone));
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "cannot read '/i/Gone.sfz': there is no such file");
    assert!(source.warnings(None).is_empty());
    assert!(source.warnings(Some(b"{ not json")).iter().any(|warning| warning.message.contains("not a valid document")));

    // Without any way to read files, naming an instrument is an error that says what to do.
    let error = make(Some(&gone), None, Purpose::Playing).err().unwrap();
    assert!(error.contains("/i/Gone.sfz") && error.contains("Builtins::with_loader"), "{error}");
    assert!(make(None, None, Purpose::Playing).is_ok());
}

#[test]
fn the_device_says_what_it_has_loaded() {
    let sfz = "<global> sw_lokey=24 sw_hikey=25 sw_default=24\n<group> sw_last=24 sw_label=Arco\n\
               <region> sample=a.wav lokey=40 hikey=80 xfin_locc1=0 xfin_hicc1=64\n<group> sw_last=25 sw_label=Pizz\n\
               <region> sample=missing.wav lokey=40 hikey=80";
    let (_, source) = made(sfz, &[("a.wav", level(100, 0.1))], Purpose::Playing);
    let state = State::new(FILE).to_bytes();
    let (_, mut controller) = make(Some(&state), Some(&source), Purpose::Playing).unwrap();
    let said = controller.sampled().unwrap();
    assert_eq!((said.name.as_str(), said.file.as_str(), said.recordings, said.regions.len()), ("Made Up", FILE, 2, 2));
    assert_eq!(said.profile.articulation_names(), ["arco", "pizz"]);
    assert_eq!(said.profile.range, Some((40, 80)));
    assert!(said.about.contains("Ways of playing, chosen by a key: arco (default) on C1, pizz on C#1."), "{}", said.about);
    assert!(Arc::ptr_eq(&said, &controller.sampled().unwrap()), "said once, kept");
    assert_eq!(controller.missing_files(), [("recording missing.wav".to_string(), "/i/missing.wav".to_string())]);
    wait_for("the starts being read", || controller.reading().is_some_and(|reading| !reading.is_loading()));
    assert_eq!(controller.reading().map(|reading| (reading.ready, reading.of)), Some((2, 2)));
    let ControllerSupport::Listed(kinds) = controller.controller_support() else { panic!() };
    assert!(kinds.contains(&ControllerKind::MOD_WHEEL) && kinds.contains(&ControllerKind::SUSTAIN));
    assert!(controller.key_sounds().is_none());
}
