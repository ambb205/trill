//! The wavetable synth, played and measured. Run with `--nocapture` to
//! see the numbers.

use super::params::PARAMS;
use super::recipe::Recipe;
use super::*;
use crate::fft::RealFft;
use crate::testing::{amplitude, largest_step, peak, rms, zero_crossing_hz};
use trill_core::device::SongTime;

const SR: f64 = 48_000.0;
const ON: fn(u8) -> EventKind = |pitch| EventKind::NoteOn { pitch, velocity: 1.0 };
const OFF: fn(u8) -> EventKind = |pitch| EventKind::NoteOff { pitch };

/// A synth set to `values`, by parameter id.
fn synth(values: &[(&str, f64)]) -> Wavetable {
    let mut synth = Wavetable::new([None, None]);
    synth.prepare(SR, 512);
    set(&mut synth, values);
    synth
}

fn set(synth: &mut Wavetable, values: &[(&str, f64)]) {
    for (id, value) in values {
        let index = PARAMS.iter().position(|def| def.id == *id).unwrap_or_else(|| panic!("no parameter '{id}'"));
        synth.set_param(index, *value);
    }
}

/// The number a choice is kept as.
fn choice(id: &str, name: &str) -> (&'static str, f64) {
    let def = PARAMS.iter().find(|def| def.id == id).unwrap_or_else(|| panic!("no parameter '{id}'"));
    let crate::params::Kind::Enum(choices) = def.kind else { panic!("{id} is no choice") };
    let index = choices.iter().position(|choice| *choice == name).unwrap_or_else(|| panic!("{id} has no '{name}'"));
    (def.id, index as f64)
}

/// Plays `device` for `frames` frames in blocks of `block`, in a song at
/// 120 that plays from its start, each event at its frame.
fn play(device: &mut Wavetable, block: usize, frames: usize, events: &[(usize, EventKind)]) -> (Vec<f32>, Vec<f32>) {
    play_from(device, block, frames, events, 0.0)
}

fn play_from(
    device: &mut Wavetable,
    block: usize,
    frames: usize,
    events: &[(usize, EventKind)],
    beat: f64,
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
            position_beats: beat + start as f64 * 120.0 / (60.0 * SR),
            song: SongTime::default(),
            loop_range: None,
        }
        .steady();
        let block_events: Vec<Event> = events
            .iter()
            .filter(|(frame, _)| (start..end).contains(frame))
            .map(|(frame, kind)| Event { offset: (frame - start) as u32, kind: *kind })
            .collect();
        device.process(&info, &block_events, &mut left[start..end], &mut right[start..end]);
        start = end;
    }
    (left, right)
}

/// One oscillator that starts and stops at once, at the same phase every
/// time and as loud whatever the velocity. A sine, unless told otherwise.
const PLAIN: [(&str, f64); 6] = [
    ("osc1_position", 0.0),
    ("osc1_phase_random", 0.0),
    ("env1_attack", 0.001),
    ("env1_sustain", 1.0),
    ("env1_release", 0.01),
    ("velocity_sens", 0.0),
];

fn with(base: &[(&'static str, f64)], more: &[(&'static str, f64)]) -> Vec<(&'static str, f64)> {
    base.iter().chain(more).copied().collect()
}

fn db(ratio: f32) -> f32 {
    20.0 * ratio.max(1e-12).log10()
}

fn hz_of(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

/// The power in each bin of `samples`, under a window with next to
/// nothing in its skirts (the square of one whose skirts are 92 dB down),
/// so that what is a hundred dB under a tone can be told from it. A tone
/// is spread over sixteen bins either side.
fn spectrum(samples: &[f32]) -> Vec<f64> {
    let size = samples.len();
    let mut plan = RealFft::new(size);
    let weights = [0.358_75, 0.488_29, 0.141_28, 0.011_68];
    let windowed: Vec<f32> = samples
        .iter()
        .enumerate()
        .map(|(n, sample)| {
            let turn = std::f64::consts::TAU * n as f64 / size as f64;
            let window = weights[0] - weights[1] * turn.cos() + weights[2] * (2.0 * turn).cos()
                - weights[3] * (3.0 * turn).cos();
            (f64::from(*sample) * window * window) as f32
        })
        .collect();
    let (mut re, mut im) = (vec![0.0f32; size / 2 + 1], vec![0.0f32; size / 2 + 1]);
    plan.forward(&windowed, &mut re, &mut im);
    re.iter().zip(&im).map(|(re, im)| f64::from(*re).powi(2) + f64::from(*im).powi(2)).collect()
}

/// How many bins either side of a tone the window spreads it over.
const SPREAD: f64 = 16.0;

/// The power on each harmonic of a steady note at `hz`, by its number.
fn lobes(samples: &[f32], hz: f64) -> Vec<f64> {
    let size = 1 << 16;
    let power = spectrum(&samples[..size]);
    let bin_hz = SR / size as f64;
    let mut lobes = vec![0.0f64; (24_000.0 / hz) as usize + 2];
    for (bin, power) in power.iter().enumerate().skip(SPREAD as usize) {
        let at = bin as f64 * bin_hz;
        let harmonic = (at / hz).round().max(1.0);
        if (at - harmonic * hz).abs() <= SPREAD * bin_hz && (harmonic as usize) < lobes.len() {
            lobes[harmonic as usize] += power;
        }
    }
    lobes
}

/// What of a steady note at `hz` lies between its harmonics, against what
/// lies on them, in dB; and the harmonic it reaches up to that is within
/// 60 dB of its strongest.
fn between_the_harmonics(samples: &[f32], hz: f64) -> (f64, f64) {
    let (all, _, reaches) = off_the_note(samples, hz);
    (all, reaches)
}

/// The same, with what lies between the harmonics under 16 kHz, where an
/// ear still hears well, counted on its own: (everywhere, under 16 kHz,
/// what the note reaches up to).
fn off_the_note(samples: &[f32], hz: f64) -> (f64, f64, f64) {
    let size = 1 << 16;
    let power = spectrum(&samples[..size]);
    let bin_hz = SR / size as f64;
    let (mut on, mut off, mut heard, mut strongest) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut lobes: Vec<f64> = Vec::new();
    for (bin, power) in power.iter().enumerate().skip(SPREAD as usize) {
        let at = bin as f64 * bin_hz;
        let harmonic = (at / hz).round().max(1.0);
        if (at - harmonic * hz).abs() <= SPREAD * bin_hz {
            on += power;
            if lobes.len() < harmonic as usize + 1 {
                lobes.resize(harmonic as usize + 1, 0.0);
            }
            lobes[harmonic as usize] += power;
            strongest = strongest.max(lobes[harmonic as usize]);
        } else {
            off += power;
            if at < 16_000.0 {
                heard += power;
            }
        }
    }
    let reaches = lobes.iter().rposition(|lobe| *lobe > strongest * 1e-6).unwrap_or(0) as f64 * hz;
    let db = |part: f64| 10.0 * (part.max(1e-30) / on.max(1e-30)).log10();
    (db(off), db(heard), reaches)
}

/// A held note of `settings` at `note`, from a moment into it: long
/// enough to take a spectrum of.
fn held(settings: &[(&'static str, f64)], note: u8) -> Vec<f32> {
    let mut synth = synth(settings);
    let (left, _) = play(&mut synth, 256, 4_800 + (1 << 16), &[(0, ON(note))]);
    left[4_800..].to_vec()
}

#[test]
fn a_note_plays_at_its_pitch_and_at_the_poly_synths_level() {
    let mut synth = synth(&PLAIN);
    let (left, right) = play(&mut synth, 256, 48_000, &[(0, ON(69))]);
    let hz = zero_crossing_hz(&left[4_800..], SR);
    assert!((hz - 440.0).abs() < 0.05, "{hz} Hz");
    assert_eq!(left, right, "one voice in the middle is the same on both sides");
    // At the default volume one voice peaks 13 dB under full scale, as
    // one voice of the poly synth does.
    let level = db(peak(&left[4_800..]));
    assert!((level + 13.15).abs() < 0.05, "{level} dBFS");
    // And it ends with its key.
    let (tail, _) = play(&mut synth, 256, 9_600, &[(0, OFF(69))]);
    assert_eq!(peak(&tail[4_800..]), 0.0);
    // Tuned by octave, semitone and cent.
    for (tuning, note) in [
        (&[("osc1_octave", -1.0)][..], 57.0),
        (&[("osc1_semitones", 7.0)][..], 76.0),
        (&[("osc1_fine", 50.0)][..], 69.5),
        (&[("osc1_octave", 2.0), ("osc1_semitones", -12.0), ("osc1_fine", -100.0)][..], 80.0),
    ] {
        let heard = zero_crossing_hz(&held(&with(&PLAIN, tuning), 69)[..24_000], SR);
        assert!((heard / hz_of(note) - 1.0).abs() < 1e-4, "{tuning:?}: {heard} Hz");
    }
}

#[test]
fn a_sawtooth_has_nothing_between_its_harmonics_at_any_pitch() {
    let saw = with(&PLAIN, &[("osc1_position", 0.5)]);
    println!("note     Hz       between the harmonics   reaches");
    let mut wrong = Vec::new();
    for note in (24u8..=120).filter(|note| note % 4 == 0 || [45, 57, 91, 99, 101, 103, 106, 115].contains(note)) {
        let hz = hz_of(f64::from(note));
        let (between, reaches) = between_the_harmonics(&held(&saw, note), hz);
        println!("{note:>4} {hz:>9.1}   {between:>8.1} dB          {reaches:>8.0} Hz");
        // What is not the note is 80 dB under it from the bottom of a
        // bass up (84 at the worst pitch there is), and more than a
        // hundred in the top octave of a piano and beyond, where it would
        // be heard soonest.
        if between > if note >= 96 { -105.0 } else { -80.0 } {
            wrong.push(format!("note {note}: {between:.1} dB between its harmonics"));
        }
        // And the note is as bright as its pitch allows: its harmonics go
        // up to within a third of an octave of half the sample rate, where
        // the table has that many to give (from 47 Hz up) and the note has
        // some to lose.
        if note >= 30 && hz < 6_000.0 && reaches < 24_000.0 * 0.98 / 1.34 - hz {
            wrong.push(format!("note {note} only reaches {reaches:.0} Hz"));
        }
        if reaches >= 24_000.0 {
            wrong.push(format!("note {note} reaches {reaches:.0} Hz"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn the_harmonics_of_a_sawtooth_are_at_their_levels() {
    // A 110 Hz sawtooth: harmonic h at 1/h of the first, up to 22 kHz.
    let samples = held(&with(&PLAIN, &[("osc1_position", 0.5)]), 45);
    let lobes = lobes(&samples, 110.0);
    for harmonic in [2usize, 3, 10, 50, 100, 170, 200] {
        let level = 10.0 * (lobes[harmonic] / lobes[1]).log10() + 20.0 * (harmonic as f64).log10();
        assert!(level.abs() < 0.1, "harmonic {harmonic}: {level:.2} dB from where it belongs");
    }
    assert!(lobes.len() <= 205 || lobes[205..].iter().all(|lobe| *lobe < lobes[1] * 1e-9), "and none over 22 kHz");
}

#[test]
fn bending_the_cycle_keeps_what_it_adds_under_the_note() {
    // What lies between the harmonics of a note two octaves over middle C
    // when the cycle is bent, by how it is bent: everywhere, and under
    // 16 kHz. A warp makes corners the table does not have, and what a
    // corner puts over half the sample rate comes back down.
    println!("warp                           between the harmonics   of that under 16 kHz");
    let sine = |more: &[(&'static str, f64)]| held(&with(&PLAIN, more), 84);
    let hz = hz_of(84.0);
    let mut wrong = Vec::new();
    let saw = ("osc1_position", 0.5);
    let fm = [("osc1_fm", 0.3), ("osc2_octave", 1.0)];
    for (name, settings, limit) in [
        ("sync 0.5 of a sine", vec![choice("osc1_warp", "sync"), ("osc1_warp_amount", 0.5)], -55.0),
        ("sync 0.5 of a saw", vec![choice("osc1_warp", "sync"), ("osc1_warp_amount", 0.5), saw], -55.0),
        ("bend 0.6 of a sine", vec![choice("osc1_warp", "bend"), ("osc1_warp_amount", 0.6)], -60.0),
        ("bend 0.6 of a saw", vec![choice("osc1_warp", "bend"), ("osc1_warp_amount", 0.6), saw], -42.0),
        ("squeeze 0.6 of a sine", vec![choice("osc1_warp", "squeeze"), ("osc1_warp_amount", 0.6)], -45.0),
        ("squeeze 0.6 of a saw", vec![choice("osc1_warp", "squeeze"), ("osc1_warp_amount", 0.6), saw], -45.0),
        ("FM 0.3 by a sine an octave up", with(&fm, &[("osc2_position", 0.0)]), -90.0),
        // A bright wave bending another: not held to anything. FM is for
        // waves with few harmonics.
        ("FM 0.3 by a saw an octave up", with(&fm, &[("osc2_position", 0.5)]), 0.0),
    ] {
        let (everywhere, heard, _) = off_the_note(&sine(&settings), hz);
        println!("{name:<30} {everywhere:>9.1} dB           {heard:>9.1} dB");
        if heard > limit {
            wrong.push(format!("{name}: {heard:.1} dB"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // With no amount a warp is not there: the plain note, to the sample.
    let plain = sine(&[]);
    for warp in ["sync", "bend", "squeeze"] {
        let warped = sine(&[choice("osc1_warp", warp)]);
        let apart = plain.iter().zip(&warped).fold(0.0f32, |most, (a, b)| most.max((a - b).abs()));
        assert!(apart < 2e-4, "{warp} at no amount is {apart} from the plain note");
    }
}

#[test]
fn a_warp_does_what_it_is_named_for() {
    let level = |settings: &[(&'static str, f64)], harmonic: f64| {
        amplitude(&held(&with(&PLAIN, settings), 57)[..48_000], 220.0 * harmonic, SR)
    };
    // Sync restarts the cycle: a sine restarted four times in its period
    // (an amount of 0.447) is mostly its fourth harmonic.
    let sync = [choice("osc1_warp", "sync"), ("osc1_warp_amount", 0.447)];
    assert!(level(&sync, 4.0) > level(&sync, 1.0) * 3.0);
    // Squeezed into half its period and resting for the other half, a
    // sine has a second harmonic as strong as its first.
    let squeeze = [choice("osc1_warp", "squeeze"), ("osc1_warp_amount", 0.5 / 0.9)];
    assert!((level(&squeeze, 2.0) / level(&squeeze, 1.0) - 1.0).abs() < 0.2);
    assert!(level(&[], 2.0) < level(&[], 1.0) * 1e-3, "and none at all without");
    // A bend leans the sine over: harmonics it did not have.
    let bend = [choice("osc1_warp", "bend"), ("osc1_warp_amount", 0.8)];
    assert!(level(&bend, 2.0) > level(&bend, 1.0) * 0.3);
    // Oscillator 2 bends oscillator 1 whatever its own level: a sine by a
    // sine an octave up gives the sidebands of FM, and nothing of
    // oscillator 2 itself is heard.
    let fm = [("osc1_fm", 0.2), ("osc2_position", 0.0), ("osc2_octave", 1.0), ("osc2_level", 0.0)];
    assert!(level(&fm, 3.0) > level(&fm, 1.0) * 0.2, "{} against {}", level(&fm, 3.0), level(&fm, 1.0));
    assert!(level(&fm, 2.0) < level(&fm, 1.0) * 0.05);
}

#[test]
fn unison_is_as_loud_as_one_voice_and_as_wide_as_it_is_spread() {
    let saw = with(&PLAIN, &[("osc1_position", 0.5), ("osc1_phase_random", 1.0)]);
    let play_chord = |more: &[(&'static str, f64)]| {
        let mut synth = synth(&with(&saw, more));
        play(&mut synth, 256, 96_000, &[(0, ON(57))])
    };
    let one = rms(&play_chord(&[]).0[9_600..]);
    println!("unison   level against one voice   sides under the middle");
    for unison in [2.0, 3.0, 4.0, 7.0, 8.0, 16.0] {
        let (left, right) = play_chord(&[("osc1_unison", unison), ("osc1_blend", 1.0), ("osc1_spread", 1.0)]);
        let both: Vec<f32> = left.iter().zip(&right).map(|(l, r)| (l * l + r * r) * 0.5).collect();
        let level = db((both[9_600..].iter().sum::<f32>() / (both.len() - 9_600) as f32).sqrt() / one);
        let mid: Vec<f32> = left.iter().zip(&right).map(|(l, r)| (l + r) * 0.5).collect();
        let side: Vec<f32> = left.iter().zip(&right).map(|(l, r)| (l - r) * 0.5).collect();
        let width = db(rms(&mid[9_600..]) / rms(&side[9_600..]));
        println!("{unison:>4}        {level:>+6.2} dB                {width:>6.1} dB");
        assert!(level.abs() < 1.0, "{unison} voices are {level:.2} dB from one");
        assert!(width < 6.0, "{unison} voices spread all the way: the sides {width:.1} dB under the middle");
    }
    // Not spread, it is the same on both sides; and the copies are where
    // the detune puts them: two voices 40 cents apart beat 5 times a
    // second at 220 Hz.
    let (left, right) = play_chord(&[("osc1_unison", 7.0), ("osc1_spread", 0.0)]);
    assert_eq!(left, right);
    let sine = with(&PLAIN, &[("osc1_unison", 2.0), ("osc1_detune", 20.0), ("osc1_spread", 0.0)]);
    let (left, _) = play(&mut synth(&sine), 256, 96_000, &[(0, ON(57))]);
    let envelope: Vec<f32> = left.chunks(480).map(peak).collect();
    let dips = envelope.windows(3).filter(|trio| trio[1] < trio[0] && trio[1] <= trio[2]).count();
    assert!((9..=11).contains(&dips), "{dips} beats in two seconds");
    // With the blend down the copy in tune stands alone.
    let alone = play_chord(&[("osc1_unison", 7.0), ("osc1_blend", 0.0), ("osc1_spread", 1.0)]);
    assert_eq!(alone.0, alone.1);
    assert!((db(rms(&alone.0[9_600..]) / one)).abs() < 0.05);
}

#[test]
fn the_same_notes_are_the_same_samples_however_the_frames_are_handed_over() {
    // Everything at once: both oscillators with unison, a warp and FM, the
    // sub and the noise, both filters (one a comb), every LFO, and a
    // matrix that moves most of it.
    let busy = [
        choice("osc1_table", "Acid"),
        ("osc1_unison", 5.0),
        choice("osc1_warp", "sync"),
        ("osc1_warp_amount", 0.3),
        ("osc1_fm", 0.15),
        choice("osc2_table", "Formant Sweep"),
        ("osc2_level", 0.7),
        ("osc2_unison", 3.0),
        ("osc2_octave", 1.0),
        choice("osc2_warp", "squeeze"),
        ("osc2_warp_amount", 0.4),
        ("sub_level", 0.5),
        choice("sub_shape", "square"),
        ("noise_level", 0.4),
        ("noise_color", -0.5),
        ("noise_width", 1.0),
        choice("filter1_type", "lowpass24"),
        ("filter1_cutoff", 900.0),
        ("filter1_resonance", 0.6),
        ("filter1_drive", 9.0),
        ("filter1_key_track", 0.5),
        choice("filter2_type", "comb"),
        ("filter2_cutoff", 330.0),
        ("filter2_resonance", 0.7),
        choice("filter_routing", "parallel"),
        ("env1_release", 0.2),
        ("env2_decay", 0.3),
        ("lfo1_rate", 6.0),
        choice("lfo2_sync", "1/8"),
        ("lfo2_retrigger", 0.0),
        choice("lfo2_shape", "steps"),
        choice("lfo3_shape", "smooth"),
        ("lfo3_rate", 9.0),
        ("lfo3_fade", 0.2),
        choice("mod1_source", "env2"),
        choice("mod1_dest", "filter1_cutoff"),
        ("mod1_amount", 0.4),
        choice("mod2_source", "lfo1"),
        choice("mod2_dest", "fine_pitch"),
        ("mod2_amount", 0.3),
        choice("mod3_source", "lfo2"),
        choice("mod3_dest", "osc1_position"),
        ("mod3_amount", 0.5),
        choice("mod4_source", "lfo3"),
        choice("mod4_dest", "filter2_cutoff"),
        ("mod4_amount", 0.1),
        choice("mod5_source", "mod_wheel"),
        choice("mod5_dest", "osc2_position"),
        ("mod5_amount", 1.0),
        choice("mod6_source", "random"),
        choice("mod6_dest", "pan"),
        ("mod6_amount", 0.6),
        choice("mod7_source", "velocity"),
        choice("mod7_dest", "lfo1_rate"),
        ("mod7_amount", 0.2),
        ("voices", 3.0),
        ("glide", 0.05),
    ];
    let hit = |pitch: u8, velocity: f32| EventKind::NoteOn { pitch, velocity };
    let events = [
        (0, hit(45, 0.9)),
        (3, hit(52, 0.5)),
        (1_000, EventKind::Control { controller: 1, value: 0.7 }),
        (2_477, hit(57, 1.0)),
        (5_001, hit(64, 0.8)),
        (5_001, EventKind::PitchBend { value: 0.5 }),
        (7_777, OFF(45)),
        (9_600, hit(61, 0.6)),
        (12_345, EventKind::Control { controller: 64, value: 1.0 }),
        (13_000, OFF(52)),
        (13_001, OFF(57)),
        (16_383, EventKind::PitchBend { value: -0.25 }),
        (16_384, hit(40, 1.0)),
        (20_000, EventKind::Control { controller: 64, value: 0.0 }),
        (22_000, EventKind::AllNotesOff),
    ];
    let run = |block: usize| play(&mut synth(&busy), block, 30_000, &events);
    let reference = run(512);
    assert!(rms(&reference.0) > 0.01 && reference.0 != reference.1);
    for block in [480, 441, 37, 16, 1, 4096] {
        assert!(run(block) == reference, "blocks of {block} frames made other samples");
    }
    // Played again from a synth that was reset, it is the same again:
    // nothing is kept from before, and what is drawn by lot is drawn from
    // the notes.
    let mut again = synth(&busy);
    play(&mut again, 512, 7_000, &[(0, hit(33, 0.3)), (100, hit(90, 1.0)), (5_000, OFF(33))]);
    again.reset();
    let replayed = play(&mut again, 512, 30_000, &events);
    let first = replayed.0.iter().zip(&reference.0).position(|(a, b)| a != b);
    assert!(replayed == reference, "after a reset the samples are others from frame {first:?}");
    // A note is not the same note twice: the copies start elsewhere.
    let two = play(&mut synth(&busy), 512, 48_000, &[(0, hit(45, 0.9)), (24_000, hit(45, 0.9))]).0;
    assert!(two[100..4_000] != two[24_100..28_000]);
}

/// The largest step from one sample to the next around `frame`, against
/// the largest anywhere in `usual`: a sound that is steady before the
/// frame and steady again well after it.
fn step_at(samples: &[f32], frame: usize, usual: std::ops::Range<usize>) -> f32 {
    let settled = largest_step(&samples[frame + 4_800..frame + 9_600]);
    largest_step(&samples[frame.saturating_sub(64)..frame + 1_200]) / largest_step(&samples[usual]).max(settled)
}

#[test]
fn nothing_clicks_when_a_voice_is_taken_or_struck_again() {
    // One voice: the second note takes it from the first, which gets out
    // of the way in five milliseconds while the new one comes in.
    let one = with(&PLAIN, &[("voices", 1.0), ("env1_attack", 0.005)]);
    let (left, _) = play(&mut synth(&one), 256, 48_000, &[(0, ON(57)), (24_000, ON(64))]);
    let taken = step_at(&left, 24_000, 12_000..20_000);
    // The new note is a fifth higher, so its steps are half as large again.
    assert!(taken < 1.6, "a step {taken} times the usual where the voice was taken");
    assert!((zero_crossing_hz(&left[30_000..], SR) / hz_of(64.0) - 1.0).abs() < 1e-3, "the new note sounds on");
    assert!(rms(&left[24_000..24_100]) > 0.02, "and the old one did not stop dead");

    // The same key again: the note carries on from where it is.
    let slow = with(&PLAIN, &[("env1_attack", 0.02), ("env1_decay", 2.0), ("env1_sustain", 0.2)]);
    let (left, _) = play(&mut synth(&slow), 256, 48_000, &[(0, ON(57)), (24_000, ON(57))]);
    assert!(step_at(&left, 24_000, 960..10_000) < 1.05);
    assert!(rms(&left[25_000..26_000]) > rms(&left[23_000..24_000]) * 1.5, "struck anew");

    // One note at a time: the note moves to the new key without a step,
    // struck anew in mono and carried on in legato.
    for (mode, struck) in [("mono", true), ("legato", false)] {
        let settings = with(&slow, &[choice("mode", mode)]);
        let (left, _) = play(&mut synth(&settings), 256, 48_000, &[(0, ON(57)), (24_000, ON(60)), (36_000, OFF(60))]);
        assert!(step_at(&left, 24_000, 960..10_000) < 1.3, "{mode}");
        let louder = rms(&left[25_000..26_000]) > rms(&left[23_000..24_000]) * 1.5;
        assert_eq!(louder, struck, "{mode}");
        assert!((zero_crossing_hz(&left[26_000..35_000], SR) / hz_of(60.0) - 1.0).abs() < 1e-3);
        // The first key is still down when the second comes up: back to it.
        assert!((zero_crossing_hz(&left[38_000..47_000], SR) / hz_of(57.0) - 1.0).abs() < 1e-3, "{mode}");
    }

    // More notes than there are voices, over and over: nothing louder
    // than the notes themselves ever comes of it.
    let few = with(&PLAIN, &[("voices", 4.0), ("osc1_position", 0.5), ("env1_release", 0.3)]);
    let events: Vec<(usize, EventKind)> = (0..200).map(|n| (n * 240, ON(40 + (n * 7 % 40) as u8))).collect();
    let (left, _) = play(&mut synth(&few), 256, 60_000, &events);
    assert!(peak(&left) < 0.22 * 5.0, "{}", peak(&left));
}

#[test]
fn nothing_clicks_when_a_parameter_is_moved_or_a_table_changed() {
    // A held sine; half a second in, something is set somewhere else.
    let moved = |settings: &[(&'static str, f64)], to: &[(&'static str, f64)]| {
        let mut synth = synth(&with(&PLAIN, settings));
        let (mut left, _) = play(&mut synth, 256, 24_000, &[(0, ON(57))]);
        set(&mut synth, to);
        left.extend(play_from(&mut synth, 256, 24_000, &[], 1.0).0);
        left
    };
    type Settings = Vec<(&'static str, f64)>;
    let cases: Vec<(&str, Settings, Settings)> = vec![
        ("the oscillator's level", vec![], vec![("osc1_level", 0.1)]),
        ("the volume", vec![], vec![("volume", -40.0)]),
        ("the pan", vec![], vec![("osc1_pan", 1.0)]),
        ("the cutoff", vec![choice("filter1_type", "lowpass24")], vec![("filter1_cutoff", 300.0)]),
        (
            "the resonance",
            vec![choice("filter1_type", "lowpass24"), ("filter1_cutoff", 400.0)],
            vec![("filter1_resonance", 0.9)],
        ),
        ("the sub", vec![], vec![("sub_level", 1.0)]),
        (
            "a macro",
            vec![choice("mod1_source", "macro1"), choice("mod1_dest", "level"), ("mod1_amount", -1.0)],
            vec![("macro1", 0.9)],
        ),
        (
            "a slot's amount",
            vec![choice("mod1_source", "velocity"), choice("mod1_dest", "level")],
            vec![("mod1_amount", -0.8)],
        ),
        (
            "the filter's kind",
            vec![choice("filter1_type", "lowpass24"), ("filter1_cutoff", 2_000.0)],
            vec![choice("filter1_type", "highpass12")],
        ),
        (
            "the routing",
            vec![choice("filter1_type", "highpass24"), ("filter1_cutoff", 5_000.0)],
            vec![choice("filter_routing", "parallel")],
        ),
        ("the warp", vec![("osc1_warp_amount", 0.7)], vec![choice("osc1_warp", "squeeze")]),
        (
            "the spread",
            vec![("osc1_unison", 5.0), ("osc1_detune", 0.0)],
            vec![("osc1_spread", 0.0), ("osc1_blend", 0.1)],
        ),
        ("the sub's shape", vec![("sub_level", 1.0)], vec![choice("sub_shape", "triangle")]),
    ];
    for (what, settings, to) in cases {
        let left = moved(&settings, &to);
        let step = step_at(&left, 24_000, 4_800..20_000);
        assert!(step < 1.25, "{what}: a step {step} times the usual");
    }
    // The position: from a sine to the square at the far end of the
    // table. The square has edges; nothing larger than its own.
    let left = moved(&[], &[("osc1_position", 0.75)]);
    let steps = largest_step(&left[23_900..24_400]);
    assert!(steps <= largest_step(&left[30_000..40_000]) * 1.05, "{steps}");

    // Another table while the note sounds: the synth goes down to
    // nothing, changes, and comes back, in a hundredth of a second.
    let left = moved(&[], &[choice("osc1_table", "FM Glass")]);
    assert!(step_at(&left, 24_000, 4_800..20_000) < 1.05);
    let dip = left[24_256..24_272].iter().fold(0.0f32, |most, sample| most.max(sample.abs()));
    assert!(dip < 1e-4, "nothing sounds at the moment of the change: {dip}");
    assert!(rms(&left[25_000..26_000]) > rms(&left[20_000..21_000]) * 0.9, "and it is back");
    // The same for anything else that cannot be changed gradually, such
    // as how many copies an oscillator has.
    let left = moved(&[], &[("osc1_unison", 7.0)]);
    assert!(left[24_256..24_272].iter().all(|sample| sample.abs() < 1e-4) && rms(&left[25_000..26_000]) > 0.05);
    // Without a note there is nothing to wait for.
    let mut silent = synth(&PLAIN);
    play(&mut silent, 256, 512, &[]);
    set(&mut silent, &[choice("osc1_table", "Organ")]);
    let (left, _) = play(&mut silent, 256, 4_800, &[(16, ON(57))]);
    assert!(rms(&left[480..960]) > 0.05);
}

#[test]
fn the_filters_do_what_they_are_named_for() {
    // What is left of each harmonic of a bright note at 220 Hz.
    let through = |settings: &[(&'static str, f64)], hz: f64| {
        let bright = with(&PLAIN, &[("osc1_position", 0.5)]);
        let open = amplitude(&held(&with(&bright, &[choice("filter1_type", "off")]), 57)[..48_000], hz, SR);
        db(amplitude(&held(&with(&bright, settings), 57)[..48_000], hz, SR) / open)
    };
    let at = |kind: &str, cutoff: f64| {
        [choice("filter1_type", kind), ("filter1_cutoff", cutoff), ("filter1_resonance", 0.0)]
    };
    // Two octaves over a low-pass at 880 Hz: 24 dB down at 12 dB an
    // octave, and twice that at 24.
    assert!((through(&at("lowpass12", 880.0), 3_520.0) + 24.0).abs() < 1.5);
    assert!((through(&at("lowpass24", 880.0), 3_520.0) + 48.0).abs() < 2.5);
    assert!(through(&at("lowpass24", 880.0), 220.0).abs() < 1.0);
    assert!((through(&at("highpass12", 3_520.0), 880.0) + 24.0).abs() < 1.5);
    assert!((through(&at("highpass24", 3_520.0), 880.0) + 48.0).abs() < 2.5);
    assert!(through(&at("highpass24", 880.0), 7_040.0).abs() < 1.0);
    // A band-pass lets its own frequency through and a notch takes it out.
    let band = at("bandpass12", 1_760.0);
    assert!(
        through(&band, 1_760.0) > through(&band, 220.0) + 12.0
            && through(&band, 1_760.0) > through(&band, 14_080.0) + 12.0
    );
    let narrow = at("bandpass24", 1_760.0);
    assert!(through(&narrow, 220.0) < through(&band, 220.0) - 10.0);
    assert!(through(&at("notch12", 1_760.0), 1_760.0) < -30.0 && through(&at("notch12", 1_760.0), 220.0).abs() < 1.0);
    assert!(through(&at("notch24", 1_760.0), 1_760.0) < -30.0);
    // Resonance raises what is at the cutoff.
    let ringing = [choice("filter1_type", "lowpass12"), ("filter1_cutoff", 1_760.0), ("filter1_resonance", 0.8)];
    assert!(through(&ringing, 1_760.0) > 15.0);
    // A comb rings at its cutoff and every multiple of it, and falls
    // between; the other comb the other way round.
    let comb = [choice("filter1_type", "comb"), ("filter1_cutoff", 440.0), ("filter1_resonance", 0.8)];
    assert!(
        through(&comb, 880.0) > through(&comb, 660.0) + 15.0
            && through(&comb, 1_760.0) > through(&comb, 1_540.0) + 15.0
    );
    let minus = [choice("filter1_type", "comb_minus"), ("filter1_cutoff", 440.0), ("filter1_resonance", 0.8)];
    assert!(through(&minus, 660.0) > through(&minus, 880.0) + 15.0);
    // The cutoff follows the key when it is told to.
    let tracked = [choice("filter1_type", "lowpass24"), ("filter1_cutoff", 440.0), ("filter1_key_track", 1.0)];
    let fifth = |settings: &[(&'static str, f64)], note: u8| {
        let bright = with(&with(&PLAIN, &[("osc1_position", 0.5)]), settings);
        let samples = held(&bright, note);
        let hz = hz_of(f64::from(note));
        db(amplitude(&samples[..48_000], hz * 5.0, SR) / amplitude(&samples[..48_000], hz, SR))
    };
    assert!((fifth(&tracked, 45) - fifth(&tracked, 81)).abs() < 1.0, "the same wave three octaves up");
    let fixed = [choice("filter1_type", "lowpass24"), ("filter1_cutoff", 440.0)];
    assert!(fifth(&fixed, 81) < fifth(&fixed, 45) - 30.0);
}

#[test]
fn drive_and_routing_shape_the_sound_as_they_say() {
    let sine = |settings: &[(&'static str, f64)]| held(&with(&PLAIN, settings), 57);
    let level = |samples: &[f32], hz: f64| amplitude(&samples[..48_000], hz, SR);
    // Drive: a sine pushed into the saturation grows a third harmonic, and
    // stays about as loud.
    let lowpass = choice("filter1_type", "lowpass24");
    let (clean, driven) = (sine(&[lowpass]), sine(&[lowpass, ("filter1_drive", 24.0), ("volume", 0.0)]));
    assert!(level(&clean, 660.0) < level(&clean, 220.0) * 1e-3);
    assert!(level(&driven, 660.0) > level(&driven, 220.0) * 0.1);
    let loud = sine(&[("volume", 0.0)]);
    assert!((db(rms(&driven) / rms(&loud))).abs() < 4.0, "{}", db(rms(&driven) / rms(&loud)));
    // A quiet sound comes out louder for it.
    let (quiet, pushed) =
        (sine(&[("osc1_level", 0.05)]), sine(&[lowpass, ("osc1_level", 0.05), ("filter1_drive", 12.0)]));
    assert!(db(rms(&pushed) / rms(&quiet)) > 6.0);
    // Side by side: a low-pass under the note and a high-pass over it let
    // nothing of a sine through either, and the balance picks one.
    let both = [
        ("osc1_position", 0.5),
        choice("filter_routing", "parallel"),
        choice("filter1_type", "lowpass24"),
        ("filter1_cutoff", 300.0),
        choice("filter2_type", "highpass24"),
        ("filter2_cutoff", 5_000.0),
    ];
    let parallel = sine(&both);
    assert!(level(&parallel, 220.0) > 0.05 && level(&parallel, 8_800.0) > 0.001 && level(&parallel, 1_100.0) < 0.002);
    let low_only = sine(&with(&both, &[("filter_balance", 0.0)]));
    assert!(level(&low_only, 8_800.0) < level(&parallel, 8_800.0) * 0.01);
    // In a row, the two leave nothing.
    let serial = sine(&with(&both, &[choice("filter_routing", "serial")]));
    assert!(rms(&serial) < rms(&parallel) * 0.02);
    // Split: oscillator 1 through the first, oscillator 2 through the second.
    let split = with(
        &both,
        &[choice("filter_routing", "split"), ("osc2_level", 1.0), ("osc2_position", 0.5), ("osc2_octave", 2.0)],
    );
    let heard = sine(&split);
    assert!(level(&heard, 220.0) > 0.05 && level(&heard, 8_800.0) > 0.005 && level(&heard, 880.0) < 0.003);
    // The sub goes past the filters when it is told to.
    let sub =
        [("osc1_level", 0.0), ("sub_level", 1.0), choice("filter1_type", "highpass24"), ("filter1_cutoff", 4_000.0)];
    assert!(level(&sine(&sub), 110.0) < 0.001);
    assert!(level(&sine(&with(&sub, &[("sub_direct", 1.0)])), 110.0) > 0.1);
    assert!(level(&sine(&with(&sub, &[("sub_direct", 1.0), ("sub_octave", -2.0)])), 55.0) > 0.1);
}

#[test]
fn the_matrix_moves_what_a_slot_says_by_what_it_says() {
    let slot = |from: &str, to: &str, amount: f64| {
        vec![choice("mod1_source", from), choice("mod1_dest", to), ("mod1_amount", amount)]
    };
    let semitones =
        |settings: &[(&'static str, f64)], events: &[(usize, EventKind)], window: std::ops::Range<usize>| {
            let mut all = vec![(0, ON(69))];
            all.extend_from_slice(events);
            let (left, _) = play(&mut synth(&with(&PLAIN, settings)), 256, 48_000, &all);
            12.0 * (zero_crossing_hz(&left[window], SR) / 440.0).log2()
        };
    let late = 24_000..47_000;
    // Pitch: 1 is 48 semitones, and 1 of the fine pitch is one.
    let wheel = [(0, EventKind::Control { controller: 1, value: 1.0 })];
    assert!((semitones(&slot("mod_wheel", "pitch", 0.25), &wheel, late.clone()) - 12.0).abs() < 0.01);
    assert!((semitones(&slot("mod_wheel", "fine_pitch", 0.5), &wheel, late.clone()) - 0.5).abs() < 0.01);
    assert!((semitones(&slot("mod_wheel", "osc1_pitch", -0.25), &wheel, late.clone()) + 12.0).abs() < 0.01);
    assert!(semitones(&slot("mod_wheel", "osc2_pitch", 0.25), &wheel, late.clone()).abs() < 0.01);
    assert!(semitones(&slot("mod_wheel", "pitch", 0.25), &[], late.clone()).abs() < 0.01, "the wheel is down");
    // What the wheels and the keys give.
    let pressed = [(0, EventKind::ChannelPressure { value: 0.5 })];
    assert!((semitones(&slot("pressure", "fine_pitch", 1.0), &pressed, late.clone()) - 0.5).abs() < 0.01);
    let breath = [(0, EventKind::Control { controller: 2, value: 1.0 })];
    assert!((semitones(&slot("breath", "fine_pitch", 1.0), &breath, late.clone()) - 1.0).abs() < 0.01);
    let bent = [(0, EventKind::PitchBend { value: -1.0 })];
    // The wheel bends by its range, and is there for the matrix as well.
    assert!((semitones(&[("bend_range", 7.0)], &bent, late.clone()) + 7.0).abs() < 0.01);
    assert!((semitones(&slot("pitch_bend", "fine_pitch", 1.0), &bent, late.clone()) + 3.0).abs() < 0.01);
    assert!((semitones(&slot("velocity", "fine_pitch", 1.0), &[], late.clone()) - 1.0).abs() < 0.01);
    // A4 is nine semitones over middle C, of the 48 that are a whole key.
    assert!((semitones(&slot("key", "fine_pitch", 1.0), &[], late.clone()) - 9.0 / 48.0).abs() < 0.01);
    assert!(
        (semitones(&with(&slot("macro3", "fine_pitch", 1.0), &[("macro3", 0.4)]), &[], late.clone()) - 0.4).abs()
            < 0.01
    );
    // An envelope: up over its attack, and back to its sustain.
    let swoop = with(&slot("env2", "pitch", 0.25), &[("env2_attack", 0.1), ("env2_decay", 0.1), ("env2_sustain", 0.0)]);
    assert!(semitones(&swoop, &[], 4_300..4_800) > 10.0);
    assert!(semitones(&swoop, &[], late.clone()).abs() < 0.05);
    // How much an envelope sends can itself be moved: by a macro, say.
    let scaled = |macro_at: f64| {
        let more = [
            choice("mod2_source", "macro1"),
            choice("mod2_dest", "env2_depth"),
            ("mod2_amount", 1.0),
            ("macro1", macro_at),
        ];
        semitones(&with(&with(&slot("env2", "pitch", 0.25), &[("env2_sustain", 1.0)]), &more), &[], late.clone())
    };
    assert!(
        (scaled(0.0) - 12.0).abs() < 0.05 && (scaled(0.5) - 18.0).abs() < 0.05 && (scaled(1.0) - 24.0).abs() < 0.05
    );
    // Two slots on one destination add up.
    let two = with(
        &slot("mod_wheel", "fine_pitch", 0.5),
        &[choice("mod2_source", "velocity"), choice("mod2_dest", "fine_pitch"), ("mod2_amount", 0.25)],
    );
    assert!((semitones(&two, &wheel, late.clone()) - 0.75).abs() < 0.01);
    // What is drawn by lot for a note is the same for that note every
    // time, and another for another.
    let lot = |pitch: u8| {
        let settings = with(&PLAIN, &slot("random", "fine_pitch", 1.0));
        let (left, _) = play(&mut synth(&settings), 256, 24_000, &[(0, ON(pitch))]);
        12.0 * (zero_crossing_hz(&left[4_800..], SR) / hz_of(f64::from(pitch))).log2()
    };
    assert_eq!(lot(69), lot(69));
    assert!(lot(69).abs() < 1.0 && (lot(69) - lot(70)).abs() > 1e-3);

    // The level, the position and the cutoff.
    let heard = |settings: &[(&'static str, f64)], velocity: f32| {
        let mut synth = synth(&with(&PLAIN, settings));
        play(&mut synth, 256, 24_000, &[(0, EventKind::NoteOn { pitch: 57, velocity })]).0
    };
    let quiet = rms(&heard(&slot("velocity", "level", -0.5), 1.0)[4_800..]) / rms(&heard(&[], 1.0)[4_800..]);
    assert!((quiet - 0.5).abs() < 0.01, "{quiet}");
    // Velocity to the position of Basic Shapes: a soft note a sine, a
    // hard one a sawtooth.
    let touch = slot("velocity", "osc1_position", 0.5);
    let third = |samples: &[f32]| amplitude(&samples[4_800..], 660.0, SR) / amplitude(&samples[4_800..], 220.0, SR);
    assert!(third(&heard(&touch, 0.0)) < 0.01 && (third(&heard(&touch, 1.0)) - 1.0 / 3.0).abs() < 0.02);
    // An envelope opening a filter four octaves and letting it fall shut.
    let pluck = with(
        &slot("env2", "filter1_cutoff", 0.4),
        &[
            ("osc1_position", 0.5),
            choice("filter1_type", "lowpass24"),
            ("filter1_cutoff", 200.0),
            ("env2_decay", 0.2),
            ("env2_sustain", 0.0),
        ],
    );
    let plucked = heard(&pluck, 1.0);
    let bright = |range: std::ops::Range<usize>| amplitude(&plucked[range], 2_200.0, SR);
    assert!(bright(480..2_400) > bright(19_200..24_000) * 30.0);
}

#[test]
fn an_lfo_runs_at_its_rate_or_in_time_with_the_song() {
    // How far the pitch strays, and how often it comes round, over two
    // seconds of a note with LFO 1 on its fine pitch.
    let wobble = |settings: &[(&'static str, f64)], events: &[(usize, EventKind)], from: f64| {
        let vibrato = [choice("mod1_source", "lfo1"), choice("mod1_dest", "fine_pitch"), ("mod1_amount", 1.0)];
        let mut all = vec![(0, ON(81))];
        all.extend_from_slice(events);
        let (left, _) = play_from(&mut synth(&with(&with(&PLAIN, &vibrato), settings)), 256, 96_000, &all, from);
        let pitches: Vec<f64> =
            left.chunks(240).map(|chunk| 12.0 * (zero_crossing_hz(chunk, SR) / 880.0).log2()).collect();
        let depth = pitches[40..].iter().fold(0.0f64, |most, pitch| most.max(pitch.abs()));
        let rises = pitches.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count();
        (depth, rises, pitches)
    };
    let (depth, cycles, _) = wobble(&[("lfo1_rate", 4.0)], &[], 0.0);
    assert!((depth - 1.0).abs() < 0.1 && (7..=9).contains(&cycles), "{depth} semitones, {cycles} cycles");
    // In time with the song: an eighth at 120 is four cycles a second, a
    // bar one every two seconds.
    let (_, cycles, _) = wobble(&[choice("lfo1_sync", "1/8")], &[], 0.0);
    assert!((7..=9).contains(&cycles), "{cycles}");
    let (_, cycles, _) = wobble(&[choice("lfo1_sync", "1 bar")], &[], 0.0);
    assert!(cycles <= 2, "{cycles}");
    // Its depth, and a wheel that brings it in.
    assert!((wobble(&[("lfo1_depth", 0.25)], &[], 0.0).0 - 0.25).abs() < 0.05);
    let wheel = [
        choice("mod2_source", "mod_wheel"),
        choice("mod2_dest", "lfo1_depth"),
        ("mod2_amount", 1.0),
        ("lfo1_depth", 0.0),
    ];
    assert!(wobble(&wheel, &[], 0.0).0 < 0.02);
    assert!((wobble(&wheel, &[(0, EventKind::Control { controller: 1, value: 0.5 })], 0.0).0 - 0.5).abs() < 0.05);
    // It fades in over as long as it is told to.
    let (_, _, pitches) = wobble(&[("lfo1_fade", 1.0), ("lfo1_rate", 8.0)], &[], 0.0);
    let reach = |range: std::ops::Range<usize>| pitches[range].iter().fold(0.0f64, |most, pitch| most.max(pitch.abs()));
    assert!(reach(10..40) < 0.25 && (reach(60..100) - 0.4).abs() < 0.15 && reach(300..400) > 0.9);
    // The shapes: a square is at one end or the other, a ramp climbs.
    let (_, _, square) = wobble(&[choice("lfo1_shape", "square"), ("lfo1_rate", 2.0)], &[], 0.0);
    let at_an_end = square[20..380].iter().filter(|pitch| (pitch.abs() - 1.0).abs() < 0.1).count();
    assert!(at_an_end > 330, "{at_an_end} of 360 readings at one end or the other");
    let (_, _, ramp) = wobble(&[choice("lfo1_shape", "ramp_up"), ("lfo1_rate", 1.0)], &[], 0.0);
    assert!(ramp[20] < ramp[100] && ramp[100] < ramp[180] && (ramp[100]).abs() < 0.1);
    // Steps: a value for each cycle, drawn from the note.
    let (_, _, steps) = wobble(&[choice("lfo1_shape", "steps"), ("lfo1_rate", 2.0)], &[], 0.0);
    assert!((steps[20] - steps[80]).abs() < 0.02 && (steps[20] - steps[120]).abs() > 0.02);
    assert_eq!(steps, wobble(&[choice("lfo1_shape", "steps"), ("lfo1_rate", 2.0)], &[], 0.0).2);

    // An LFO that does not retrigger and keeps time is where the song is:
    // a song joined half a beat in finds a quarter-note LFO half way.
    let kept = [choice("lfo1_sync", "1/4"), ("lfo1_retrigger", 0.0), choice("lfo1_shape", "ramp_up")];
    let (_, _, from_start) = wobble(&kept, &[], 0.0);
    let (_, _, joined) = wobble(&kept, &[], 0.5);
    assert!((from_start[2] + 1.0).abs() < 0.15, "at the start of its cycle: {}", from_start[2]);
    assert!(joined[2].abs() < 0.15, "half way through it: {}", joined[2]);
    // One that retriggers starts with its note wherever the song is.
    let own = [choice("lfo1_sync", "1/4"), choice("lfo1_shape", "ramp_up")];
    assert!((wobble(&own, &[], 0.5).2[2] + 1.0).abs() < 0.15);
    // And one that runs on is the same for a second note as for the first.
    let free = [("lfo1_retrigger", 0.0), ("lfo1_rate", 0.4), choice("lfo1_shape", "ramp_up")];
    let (_, _, second) = wobble(&free, &[(48_000, ON(81))], 0.0);
    assert!(second[230].abs() < 0.25, "the second note finds the LFO where it is: {}", second[230]);
}

#[test]
fn glide_slides_the_pitch_and_legato_only_between_notes_that_overlap() {
    let pitch_at = |settings: &[(&'static str, f64)], events: &[(usize, EventKind)], frame: usize| {
        let (left, _) = play(&mut synth(&with(&PLAIN, settings)), 256, 60_000, events);
        12.0 * (zero_crossing_hz(&left[frame..frame + 960], SR) / 440.0).log2()
    };
    let octave = [(0, ON(57)), (12_000, OFF(57)), (24_000, ON(69))];
    let overlapping = [(0, ON(57)), (24_000, ON(69))];
    let slow = [("glide", 0.4)];
    // No glide: there at once.
    assert!(pitch_at(&[], &octave, 24_100).abs() < 0.05);
    // With one, a note comes from the last one played, in poly and in mono.
    for mode in ["poly", "mono"] {
        let settings = with(&slow, &[choice("mode", mode)]);
        let early = pitch_at(&settings, &octave, 24_500);
        assert!((-11.5..-6.0).contains(&early), "{mode}: {early} semitones a moment into the slide");
        assert!(pitch_at(&settings, &octave, 58_000).abs() < 0.15, "{mode}: it gets there");
    }
    // Legato: only a note played while another is held slides.
    let legato = with(&slow, &[choice("mode", "legato")]);
    assert!(pitch_at(&legato, &octave, 24_500).abs() < 0.05, "the key came up first: no slide");
    assert!((-11.5..-6.0).contains(&pitch_at(&legato, &overlapping, 24_500)));
}

#[test]
fn the_sub_and_the_noise_are_what_they_are_set_to() {
    let only = |settings: &[(&'static str, f64)]| held(&with(&with(&PLAIN, &[("osc1_level", 0.0)]), settings), 57);
    // An octave under the note, at an oscillator's level.
    let sub = only(&[("sub_level", 1.0)]);
    assert!((zero_crossing_hz(&sub[..24_000], SR) - 110.0).abs() < 0.1);
    assert!((db(peak(&sub)) + 13.15).abs() < 0.1);
    for (shape, third) in [("triangle", 1.0 / 9.0), ("saw", 1.0 / 3.0), ("square", 1.0 / 3.0)] {
        let wave = only(&[("sub_level", 1.0), choice("sub_shape", shape)]);
        let level = amplitude(&wave[..48_000], 330.0, SR) / amplitude(&wave[..48_000], 110.0, SR);
        assert!((level - third).abs() < 0.02, "{shape}: its third harmonic at {level}");
    }
    // Noise: 13 dB under an oscillator at its peak, dark or thin as it is
    // coloured and about as loud either way, and as wide as it is set.
    let white = only(&[("noise_level", 1.0)]);
    // (Noise that reaches as high as a wave of full level is 4.8 dB under
    // that on average.)
    assert!((db(rms(&white)) + 13.15 + 13.0 + 4.8).abs() < 1.0, "{}", db(rms(&white)));
    let weight = |samples: &[f32], low: bool| {
        let power = spectrum(&samples[..1 << 15]);
        let split = 2_000 * (1 << 15) / 48_000;
        let part: f64 = if low { power[..split].iter().sum() } else { power[split..].iter().sum() };
        10.0 * (part / power.iter().sum::<f64>()).log10()
    };
    let (dark, thin) =
        (only(&[("noise_level", 1.0), ("noise_color", -1.0)]), only(&[("noise_level", 1.0), ("noise_color", 1.0)]));
    assert!(weight(&dark, true) > -0.5 && weight(&white, true) < -9.0 && weight(&thin, true) < -25.0);
    assert!((db(rms(&dark) / rms(&white))).abs() < 3.0, "{}", db(rms(&dark) / rms(&white)));
    let wide = play(
        &mut synth(&with(&PLAIN, &[("osc1_level", 0.0), ("noise_level", 1.0), ("noise_width", 1.0)])),
        256,
        48_000,
        &[(0, ON(57))],
    );
    let dot: f32 = wide.0.iter().zip(&wide.1).map(|(l, r)| l * r).sum();
    assert!((dot / (rms(&wide.0) * rms(&wide.1) * 48_000.0)).abs() < 0.05);
    // The same noise for the same note, every time.
    assert_eq!(only(&[("noise_level", 1.0)]), white);
}

#[test]
fn an_oscillator_plays_a_table_of_its_own_and_goes_without_where_there_is_none() {
    // A table that is one harmonic: the fifth.
    let recipe: Recipe =
        serde_json::from_str(r#"{ "name": "Fifth", "frames": [ {"harmonics": [0, 0, 0, 0, 1]} ] }"#).unwrap();
    let own = Arc::new(recipe.table());
    let mut synth = Wavetable::new([Some(own), None]);
    synth.prepare(SR, 512);
    set(&mut synth, &with(&PLAIN, &[choice("osc1_table", "custom")]));
    let (left, _) = play(&mut synth, 256, 24_000, &[(0, ON(45))]);
    assert!((zero_crossing_hz(&left[4_800..], SR) - 550.0).abs() < 0.1);
    // An oscillator told to play its own table that has none plays the
    // first that ships.
    let mut without = self::synth(&with(&PLAIN, &[choice("osc1_table", "custom")]));
    let (left, _) = play(&mut without, 256, 24_000, &[(0, ON(45))]);
    assert!((zero_crossing_hz(&left[4_800..], SR) - 110.0).abs() < 0.1);
}

#[test]
fn a_state_is_read_and_written_and_names_its_files() {
    let document = br#"{ "version": 1, "macros": ["Brightness", "", "Bite"],
        "osc1": { "name": "Reed", "size": 8, "frames": [ {"shape":"pulse","width":0.5}, {"shape":"pulse","width":0.1} ] },
        "osc2": { "file": "C:/Tables/glass.wav", "cycle": 2048 },
        "later": { "kept": true } }"#;
    let state = State::from_bytes(document).unwrap();
    assert!(
        matches!(state.table(0), Some(TableSource::Recipe(recipe)) if recipe.name == "Reed" && recipe.frames.len() == 2)
    );
    assert!(matches!(state.table(1), Some(TableSource::File(spec)) if spec.cycle == Some(2048)));
    assert_eq!(
        (state.table(0).unwrap().name(), state.table(1).unwrap().name()),
        ("Reed".to_string(), "glass".to_string())
    );
    assert_eq!([state.macro_name(0), state.macro_name(1), state.macro_name(3)], ["Brightness", "Macro 2", "Macro 4"]);
    // Written back, it is what it was, with what this build does not know.
    let again = State::from_bytes(&state.to_bytes()).unwrap();
    assert_eq!(again, state);
    assert_eq!(again.extra["later"]["kept"], true);
    // Its files are listed, and found again elsewhere.
    let files = sample_refs(Some(document)).unwrap();
    assert_eq!(files, [SampleRef { slot: "osc2 table".into(), path: "C:/Tables/glass.wav".into() }]);
    let moved = relinked(Some(document), &[("osc2 table".into(), "D:/Tables/glass.wav".into())]).unwrap();
    assert_eq!(sample_refs(Some(&moved)).unwrap()[0].path, "D:/Tables/glass.wav");
    assert_eq!(State::from_bytes(&moved).unwrap().table(0), state.table(0), "and nothing else is touched");
    for slot in ["osc1 table", "sample"] {
        let error = relinked(Some(document), &[(slot.into(), "x.wav".into())]).unwrap_err().to_string();
        assert!(error.contains("has no file in"), "{error}");
    }
    // No state is a synth with nothing of its own.
    assert!(State::from_device_state(None).unwrap().is_empty() && sample_refs(None).unwrap().is_empty());
    assert!(!state.is_empty());
    assert!(matches!(State::from_bytes(br#"{ "version": 9 }"#), Err(StateError::TooNew { found: 9, .. })));
}

/// How much of one core the synth takes to play `notes` notes of
/// `unison` copies on each of `oscillators` oscillators, in blocks of 256
/// frames at 48 kHz.
fn share_of_a_core(notes: usize, unison: f64, oscillators: usize, more: &[(&'static str, f64)]) -> f64 {
    let settings = [
        ("osc1_position", 0.5),
        ("osc1_unison", unison),
        ("osc2_unison", unison),
        ("osc2_position", 0.5),
        ("osc2_level", if oscillators > 1 { 1.0 } else { 0.0 }),
        ("voices", 32.0),
        ("env1_sustain", 1.0),
    ];
    let mut synth = synth(&with(&settings, more));
    let events: Vec<(usize, EventKind)> = (0..notes).map(|n| (0, ON(36 + (n * 5 % 48) as u8))).collect();
    play(&mut synth, 256, 4_800, &events);
    let seconds = 4.0;
    let started = std::time::Instant::now();
    let (left, _) = play_from(&mut synth, 256, (seconds * SR) as usize, &[], 0.2);
    let took = started.elapsed().as_secs_f64();
    assert!(peak(&left) > 0.0);
    took / seconds
}

/// Not a test of anything: the table the report quotes. Run it alone,
/// with `--ignored --nocapture`, on a machine that is doing nothing else.
#[test]
#[ignore = "a measurement, for a machine that is otherwise idle: cargo test -p trill-dsp --lib share_of_a_core -- --ignored --nocapture"]
fn the_share_of_a_core_the_synth_takes() {
    println!("notes x unison x oscillators                          share of one core");
    let filters = [choice("filter1_type", "lowpass24"), ("filter1_cutoff", 2_000.0)];
    let both = with(
        &filters,
        &[choice("filter2_type", "comb"), ("filter2_cutoff", 500.0), choice("filter_routing", "parallel")],
    );
    let warped = with(&filters, &[choice("osc1_warp", "sync"), ("osc1_warp_amount", 0.4), ("osc1_fm", 0.2)]);
    for (name, notes, unison, oscillators, more) in [
        ("1 note, 1 voice, 1 oscillator, one filter", 1, 1.0, 1, &filters[..]),
        ("8 notes, 1 voice, 1 oscillator, one filter", 8, 1.0, 1, &filters[..]),
        ("8 notes of 8 unison, 1 oscillator, one filter", 8, 8.0, 1, &filters[..]),
        ("8 notes of 8 unison, 2 oscillators, one filter", 8, 8.0, 2, &filters[..]),
        ("8 notes of 8 unison, 2 oscillators, both filters", 8, 8.0, 2, &both[..]),
        ("8 notes of 8 unison, 2 oscillators, sync and FM", 8, 8.0, 2, &warped[..]),
        ("8 notes of 16 unison, 2 oscillators, one filter", 8, 16.0, 2, &filters[..]),
        ("16 notes of 16 unison, 2 oscillators, one filter", 16, 16.0, 2, &filters[..]),
        ("32 notes of 16 unison, 2 oscillators, one filter", 32, 16.0, 2, &filters[..]),
    ] {
        let share = share_of_a_core(notes, unison, oscillators, more);
        println!("{name:<52} {:>6.1} %", share * 100.0);
    }
}

#[test]
fn eight_notes_of_eight_voices_leave_most_of_a_core() {
    // Far under what a machine that is busy elsewhere would make of it:
    // this fails only if the synth has become several times as costly.
    let filters = [choice("filter1_type", "lowpass24"), ("filter1_cutoff", 2_000.0)];
    let share = share_of_a_core(8, 8.0, 2, &filters);
    assert!(share < 0.6, "{:.0} % of a core", share * 100.0);
}

/// The patch `name` on a synth, with its macros where `macros` puts them.
fn patch(name: &str) -> Wavetable {
    let patch = patches::named(name).unwrap_or_else(|| panic!("no patch '{name}'"));
    let mut synth = Wavetable::new([None, None]);
    synth.prepare(SR, 512);
    for (id, value) in patch.values() {
        let index = PARAMS.iter().position(|def| def.id == id).unwrap();
        synth.set_param(index, value);
    }
    synth
}

/// What a patch is played to be measured, held for some seconds: a note
/// where it is written if it plays one note at a time (a bass low, a lead
/// high), and a chord if it plays several.
fn phrase(category: &str, one_at_a_time: bool) -> (Vec<u8>, f64) {
    match (one_at_a_time, category) {
        (true, "Bass") => (vec![40], 1.5),
        (true, _) => (vec![72], 1.5),
        (false, "Pad") => (vec![48, 55, 60, 64], 5.0),
        (false, _) => (vec![48, 55, 60, 64], 1.5),
    }
}

/// How loud `left` and `right` are over their loudest 400 ms, in dBFS,
/// the two sides averaged; and their peak.
fn loudest(left: &[f32], right: &[f32]) -> (f32, f32) {
    let power: Vec<f32> = left.iter().zip(right).map(|(l, r)| 0.5 * (l * l + r * r)).collect();
    let window = (0.4 * SR) as usize;
    let loudest = (0..power.len() - window)
        .step_by(2_400)
        .map(|start| power[start..start + window].iter().sum::<f32>() / window as f32)
        .fold(0.0f32, f32::max);
    (10.0 * loudest.max(1e-12).log10(), db(peak(left).max(peak(right))))
}

/// How loud a patch is playing its phrase at a velocity of 100.
fn loudness(patch: &patches::Patch) -> (f32, f32) {
    let one_at_a_time = patch.params.iter().any(|(id, value)| *id == "mode" && *value != patches::Value::C("poly"));
    let (notes, seconds) = phrase(patch.category, one_at_a_time);
    let hit = |pitch: u8| EventKind::NoteOn { pitch, velocity: 100.0 / 127.0 };
    let events: Vec<(usize, EventKind)> = notes.iter().map(|note| (0, hit(*note))).collect();
    let (left, right) = play(&mut self::patch(patch.name), 256, (seconds * SR) as usize, &events);
    loudest(&left, &right)
}

/// What every patch is matched to, in dBFS over its loudest 400 ms: where
/// the poly synth's presets lie by the same measure (most of them between
/// -17 and -14). And the room a patch leaves over its phrase: one that
/// would have less at that level (a pluck, whose start is far over its
/// ring) is turned down until it has it.
const PATCH_LEVEL: f32 = -16.5;
const PATCH_ROOM: f32 = -4.0;
const PATCH_TOLERANCE: f32 = 1.0;

#[test]
fn the_patches_are_as_loud_as_one_another_and_leave_room() {
    println!("patch            category   loudest 400 ms    peak        volume to match");
    let mut wrong = Vec::new();
    for patch in patches::PATCHES.iter().skip(1) {
        let (level, peak) = loudness(patch);
        // To the level, or as near it as leaves the room.
        let wanted = (PATCH_LEVEL - level).min(PATCH_ROOM - peak);
        let volume = patch.values().iter().find(|(id, _)| id == "volume").map_or(-21.0, |(_, value)| *value);
        println!(
            "{:<16} {:<8}   {level:>7.1} dBFS   {peak:>6.1} dBFS   {:>6.1}",
            patch.name,
            patch.category,
            volume + f64::from(wanted)
        );
        if wanted.abs() > PATCH_TOLERANCE {
            wrong.push(format!(
                "{} is at {level:.1} dBFS, peak {peak:.1}: its volume wants {wanted:+.1} dB",
                patch.name
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn every_patch_sounds_ends_and_answers_its_macros() {
    for patch in patches::PATCHES {
        let mut synth = self::patch(patch.name);
        let (left, right) = play(&mut synth, 256, 48_000, &[(0, ON(57)), (3, ON(64))]);
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()), "{}", patch.name);
        assert!(rms(&left[..24_000]) > 0.003, "{} is silent", patch.name);
        assert!(peak(&left) < 1.5, "{} is too loud: {}", patch.name, peak(&left));
        // Let go of, it ends within its release, and the longest is a pad's.
        let (tail, _) = play_from(&mut synth, 256, 30 * 48_000, &[(0, OFF(57)), (0, OFF(64))], 2.0);
        assert_eq!(peak(&tail[tail.len() - 4_800..]), 0.0, "{} never ends", patch.name);
        // Every macro it names changes the sound.
        let plain = play(&mut self::patch(patch.name), 256, 24_000, &[(0, ON(57))]).0;
        for (which, name) in patch.macros.iter().enumerate().filter(|(_, name)| !name.is_empty()) {
            let mut moved = self::patch(patch.name);
            set(&mut moved, &[(["macro1", "macro2", "macro3", "macro4"][which], 1.0)]);
            let heard = play(&mut moved, 256, 24_000, &[(0, ON(57))]).0;
            let apart = plain.iter().zip(&heard).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(apart > 0.002, "{}: {name} changes nothing ({apart})", patch.name);
        }
    }
}

/// Not a test: how loud the poly synth's presets are by the same measure,
/// which is where the level the patches are matched to comes from.
#[test]
#[ignore = "a measurement: cargo test -p trill-dsp --lib the_poly_synths_presets -- --ignored --nocapture"]
fn how_loud_the_poly_synths_presets_are_by_the_same_measure() {
    use crate::synth::{PARAMS as SYNTH_PARAMS, PRESETS, PolySynth};
    for preset in PRESETS.iter().skip(1) {
        let category = match preset.name {
            "bass" | "sub_bass" | "acid_bass" => "Bass",
            "lead" => "Lead",
            "warm_pad" | "strings" => "Pad",
            _ => "Keys",
        };
        let mut synth = PolySynth::new();
        synth.prepare(SR, 512);
        for (id, value) in preset.params {
            synth.set_param(SYNTH_PARAMS.iter().position(|def| def.id == *id).unwrap(), *value);
        }
        let one_at_a_time = preset.params.iter().any(|(id, value)| *id == "mono" && *value == 1.0);
        let (notes, seconds) = phrase(category, one_at_a_time);
        let hit = |pitch: u8| EventKind::NoteOn { pitch, velocity: 100.0 / 127.0 };
        let events: Vec<(usize, EventKind)> = notes.iter().map(|note| (0, hit(*note))).collect();
        let (left, right) = crate::testing::run(&mut synth, SR, 256, (seconds * SR) as usize, &events);
        let (level, peak) = loudest(&left, &right);
        println!("{:<12} {category:<5} {level:>7.1} dBFS   peak {peak:>6.1} dBFS", preset.name);
    }
}
