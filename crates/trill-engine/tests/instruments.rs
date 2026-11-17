//! What the built-in instruments do with a note, proved on renders: how
//! loud a hit of a velocity is, where a synth's pitch goes at the start of
//! a note and under the wheel, what its noise is, and where a level ends.
//!
//! The targets are what Ableton Live's Simpler and Operator were measured
//! to do on the bench's second song (`trill-ab`, report of 17 November
//! 2026, sections 4, 5.5 and 6.4, and its recordings of each instrument
//! alone): those numbers are printed beside Trill's. Run with `--nocapture`
//! to see the tables.

mod common;

use common::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_core::controllers::ControllerKind;
use trill_core::project::{AutomationPoint, AutomationTarget, FORMAT_VERSION};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId};
use trill_dsp::{Builtins, drumrack, sampler};
use trill_engine::render::{RenderOptions, Rendered, render};
use trill_engine::{AudioBackend, Session};
use trill_media::{MediaCache, WavFormat};

/// A beat is half a second.
const TEMPO: f64 = 120.0;

fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

/// `beats` as a range of frames.
fn beats(range: std::ops::Range<f64>) -> std::ops::Range<usize> {
    (range.start * 0.5 * SR).round() as usize..(range.end * 0.5 * SR).round() as usize
}

fn note(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

/// A song of one track: an instrument and the notes it plays.
struct Played {
    project: Project,
    track: TrackId,
    device: DeviceId,
    factory: Builtins,
}

impl Played {
    /// `uri` set to `params`, with `state` if it is a sample player,
    /// playing `notes` in a clip of `length` beats.
    fn new(uri: &str, params: &[(&str, f64)], state: Option<Vec<u8>>, notes: &[Note], length: f64) -> Self {
        let mut project = Project::new("instruments");
        project.master.devices.clear();
        project.set_tempo(TEMPO).unwrap();
        let track = project.add_track("Played");
        let device = project.add_device(ChainRef::Track(track), uri, "Instrument", None).unwrap();
        let params: BTreeMap<String, f64> = params.iter().map(|(id, value)| (id.to_string(), *value)).collect();
        project.device_mut(device).unwrap().params = params;
        project.device_mut(device).unwrap().state = state;
        let clip = project.add_midi_clip(track, "clip", 0.0, length).unwrap();
        project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
        Self { project, track, device, factory: Builtins::with_loader(Arc::new(MediaCache::default())) }
    }

    fn synth(params: &[(&str, f64)], notes: &[Note], length: f64) -> Self {
        Self::new("builtin:polysynth", params, None, notes, length)
    }

    fn render(&self) -> Rendered {
        self.render_this(&self.project)
    }

    fn render_this(&self, project: &Project) -> Rendered {
        let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
        let out = render(project, &self.factory, &options).unwrap();
        assert!(out.problems.is_empty(), "{:?}", out.problems);
        out
    }

    /// The song as a build of format 2 saved it, opened again.
    fn as_saved_in_format_2(&self) -> Project {
        let file = self.project.to_json();
        let now = format!("\"format_version\": {FORMAT_VERSION}");
        assert!(file.contains(&now));
        Project::from_json(&file.replace(&now, "\"format_version\": 2")).unwrap()
    }

    fn automate(&mut self, param: &str, points: &[(f64, f64)]) {
        let target = AutomationTarget::DeviceParam { device: self.device, param: param.to_string() };
        let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
        self.project.set_automation(target, points).unwrap();
    }
}

/// A hit whose first sample is its loudest, at `peak`: a file a sample
/// player's level can be read from to the hundredth of a dB.
fn hit(folder: &tempfile::TempDir, peak: f32) -> String {
    let samples: Vec<f32> = (0..9_600).map(|n| peak * (-(n as f32) / 1_500.0).exp()).collect();
    let path = file(folder, "hit.wav");
    let buffer = AudioBuffer { sample_rate: SR, left: samples.clone(), right: samples };
    trill_media::write_wav(&path, &buffer, WavFormat::Float32).unwrap();
    path.display().to_string()
}

/// The velocities a hi-hat's rolls were measured at, a beat apart.
const VELOCITIES: [u8; 5] = [46, 60, 75, 100, 110];

fn roll() -> Vec<Note> {
    VELOCITIES.iter().enumerate().map(|(index, velocity)| note(60, index as f64, 0.25, *velocity)).collect()
}

/// What each hit of [`roll`] peaks at, in dB against `peak`.
fn hit_levels(out: &Rendered, peak: f32) -> Vec<f64> {
    (0..VELOCITIES.len())
        .map(|index| db(f64::from(common::peak(&out.left[beats(index as f64..index as f64 + 0.9)]) / peak)))
        .collect()
}

#[test]
fn a_sampler_plays_a_roll_as_loud_as_lives_simpler_was_measured_to() {
    let folder = folder();
    let path = hit(&folder, 0.25);
    // The song's hi-hat, as it is set in Live and as it translates: how
    // much velocity changes the level at 80 %, the volume at -12 dB.
    let hat = [("one_shot", 1.0), ("release", 0.02), ("velocity_sens", 0.8), ("volume", -12.0)];
    let state = sampler::State::new(path.as_str()).to_bytes();
    let played = Played::new("builtin:sampler", &hat, Some(state), &roll(), 6.0);
    let levels = hit_levels(&played.render(), 0.25);
    // Live's Simpler, against its Volume setting (report, 5.5).
    let live = [-9.3, -4.8, 0.0, 8.0, 11.2];
    println!("velocity   Live's Simpler at 80 %   Trill's sampler at 0.8   (dB against the volume setting)");
    for ((velocity, live), level) in VELOCITIES.iter().zip(live).zip(&levels) {
        println!("  {velocity:>4}         {live:>+6.1}                  {:>+6.2}", level + 12.0);
        assert!((level + 12.0 - live).abs() < 0.1, "velocity {velocity}: {} dB against the volume", level + 12.0);
    }
    // The roll swells 17 dB from velocity 46 to 100, and a hard hit is
    // louder than the volume says.
    assert!((levels[3] - levels[0] - 17.3).abs() < 0.05, "{}", levels[3] - levels[0]);
    assert!(levels[4] > -12.0 + 11.0);

    // The same song saved before the law changed plays as it did: never
    // above the volume, and a roll that swells 7 dB.
    let old = hit_levels(&played.render_this(&played.as_saved_in_format_2()), 0.25);
    for (velocity, level) in VELOCITIES.iter().zip(&old) {
        let was = -12.0 + db(0.2 + 0.8 * (f64::from(*velocity) / 127.0).powi(2));
        assert!((level - was).abs() < 0.01, "velocity {velocity}: {level} dB, and was {was}");
    }
    assert!((old[3] - old[0] - 7.2).abs() < 0.1 && old[4] < -12.0);
}

#[test]
fn a_drum_rack_follows_velocity_by_the_sampler_s_law() {
    let folder = folder();
    let path = hit(&folder, 0.25);
    let mut state = drumrack::State::default();
    state.set_pad(36, path.as_str()).unwrap();
    let hits: Vec<Note> = [30u8, 75, 100, 127].iter().enumerate().map(|(n, v)| note(36, n as f64, 0.25, *v)).collect();
    let level = |params: &[(&str, f64)], project: Option<fn(&Played) -> Project>| -> Vec<f64> {
        let played = Played::new("builtin:drumrack", params, Some(state.to_bytes()), &hits, 5.0);
        let out = match project {
            Some(saved) => played.render_this(&saved(&played)),
            None => played.render(),
        };
        (0..4).map(|n| db(f64::from(peak(&out.left[beats(n as f64..n as f64 + 0.9)]) / 0.25))).collect()
    };
    // As it comes: half of 0.4 dB a step about velocity 75, where a hit is
    // at the volume, 8 dB under the file.
    let levels = level(&[], None);
    for (wanted, level) in [-17.0, -8.0, -3.0, 2.4].iter().zip(&levels) {
        assert!((level - wanted).abs() < 0.05, "{levels:?}");
    }
    // At full sensitivity it is the sampler's law to the letter.
    let full = level(&[("velocity_sens", 1.0), ("volume", 0.0)], None);
    for (wanted, level) in [-18.0, 0.0, 10.0, 20.8].iter().zip(&full) {
        assert!((level - wanted).abs() < 0.05, "{full:?}");
    }
    // A rack saved before plays every hit at or under its volume, as it did.
    let old = level(&[], Some(Played::as_saved_in_format_2));
    for (velocity, level) in [30.0f64, 75.0, 100.0, 127.0].iter().zip(&old) {
        let was = db((velocity / 127.0).powf(1.5));
        assert!((level - was).abs() < 0.01, "velocity {velocity}: {level} dB, and was {was}");
    }
}

/// The settings of the 808 the bench's second song plays on Live's
/// Operator: a sine, 60 % of its level from velocity, at -10 dB.
const SUB: [(&str, f64); 7] = [
    ("osc1_wave", 0.0),
    ("attack", 0.001),
    ("decay", 20.0),
    ("sustain", 1.0),
    ("release", 0.12),
    ("velocity_sens", 0.6),
    ("volume", -10.0),
];

#[test]
fn a_sine_at_a_volume_setting_is_as_loud_as_operators_at_the_same() {
    // What Live's Operator peaked at on the song's sub, alone, by the
    // velocity of the note (the bench's recording of it): -3.5 dBFS at 112,
    // -2.4 dBFS at 124.
    let live = [(112u8, -3.52), (124, -2.37)];
    println!("velocity   Live's Operator   Trill's poly synth   (peak, dBFS, volume at -10 dB)");
    for (velocity, live) in live {
        let played = Played::synth(&SUB, &[note(45, 0.0, 2.0, velocity)], 3.0);
        let level = db(f64::from(peak(&played.render().left[beats(0.5..2.0)])));
        println!("  {velocity:>4}       {live:>+6.2}             {level:>+6.2}");
        assert!((level - live).abs() < 0.5, "velocity {velocity}: {level} dBFS against Live's {live}");
    }
    // The same synth in a song saved before is where it was, 21 dB down
    // that scale: a volume of -10 dB then is -31 dB now.
    let played = Played::synth(&SUB, &[note(45, 0.0, 2.0, 127)], 3.0);
    let old = played.as_saved_in_format_2();
    assert_eq!(old.device(played.device).unwrap().params.get("volume"), Some(&-31.0));
    let level = db(f64::from(peak(&played.render_this(&old).left[beats(0.5..2.0)])));
    assert!((level - db(0.22 * 10f64.powf(-0.5))).abs() < 0.01, "{level} dBFS");
}

#[test]
fn a_pitch_envelope_drops_every_note_onto_its_pitch_as_an_envelope_decays() {
    // How far over its own pitch a note on A4 is, a hundredth of a second
    // around a moment.
    let over = |out: &Rendered, start: usize, seconds: f64| {
        let from = start + ((seconds - 0.005) * SR) as usize;
        12.0 * (zero_crossing_hz(&out.left[from..from + (0.01 * SR) as usize], SR) / 440.0).log2()
    };
    // An octave over three tenths of a second: half of the way is gone
    // after a tenth of the time, nine tenths after a third, and then the
    // note is on its pitch. Every note falls anew.
    let slow: Vec<(&str, f64)> = SUB.iter().copied().chain([("pitch_env", 12.0), ("pitch_decay", 0.3)]).collect();
    let played = Played::synth(&slow, &[note(69, 0.0, 1.5, 110), note(69, 2.0, 1.5, 110)], 4.0);
    let out = played.render();
    for start in [0, beats(2.0..2.0).start] {
        for (seconds, wanted) in [(0.01, 9.5), (0.03, 6.0), (0.1, 1.2), (0.2, 0.12), (0.5, 0.0)] {
            let heard = over(&out, start, seconds);
            assert!((heard - wanted).abs() < 0.2, "{seconds} s into the note: {heard} semitones over, wanted {wanted}");
        }
    }

    // An 808's knock, as the bench's song sets it on Live's Operator: an
    // octave up, a pitch envelope of 30 ms. What Operator's pitch was read
    // to be from the bench's recording of it alone, beside the law here.
    let live = [(2.0, 6.5), (4.0, 3.3), (6.0, 2.1), (10.0, 1.2), (15.0, 0.1), (20.0, 0.0)];
    println!("ms into the note   Live's Operator   Trill's law   (semitones over the note)");
    for (ms, live) in live {
        let law = 12.0 * (-(1000f64.ln()) * ms / 30.0).exp();
        println!("      {ms:>4}             {live:>+5.1}         {law:>+5.1}");
        assert!((law - live).abs() < 1.6, "{ms} ms: {law} against Live's {live}");
    }
    let knock: Vec<(&str, f64)> = SUB.iter().copied().chain([("pitch_env", 12.0), ("pitch_decay", 0.03)]).collect();
    let out = Played::synth(&knock, &[note(69, 0.0, 1.0, 110)], 2.0).render();
    // It starts well over the note, and has landed after 20 ms.
    let first = zero_crossing_hz(&out.left[..(0.004 * SR) as usize], SR);
    assert!(first > 440.0 * 1.4, "{first} Hz over the first 4 ms");
    let landed = zero_crossing_hz(&out.left[(0.02 * SR) as usize..(0.4 * SR) as usize], SR);
    assert!((landed - 440.0).abs() < 0.5, "{landed} Hz after the fall");

    // A rise from below, and no envelope at all where the amount is nothing.
    let rising: Vec<(&str, f64)> = SUB.iter().copied().chain([("pitch_env", -12.0), ("pitch_decay", 0.3)]).collect();
    let out = Played::synth(&rising, &[note(69, 0.0, 1.0, 110)], 2.0).render();
    assert!((over(&out, 0, 0.03) + 6.0).abs() < 0.2, "{} semitones", over(&out, 0, 0.03));
    let out = Played::synth(&SUB, &[note(69, 0.0, 1.0, 110)], 2.0).render();
    assert!((zero_crossing_hz(&out.left[48..(0.03 * SR) as usize], SR) - 440.0).abs() < 2.0);
}

#[test]
fn the_pitch_wheel_reaches_as_far_as_it_is_set_to_and_the_track_knows() {
    let wide: Vec<(&str, f64)> = SUB.iter().copied().chain([("bend_range", 12.0)]).collect();
    let pitch = |params: &[(&str, f64)], wheel: f64| {
        let mut played = Played::synth(params, &[note(69, 0.0, 3.0, 110)], 4.0);
        let clip = played.project.track(played.track).unwrap().clips[0].id;
        let curve = vec![AutomationPoint::new(0.0, 0.0), AutomationPoint::new(1.0, wheel)];
        played.project.clip_mut(clip).unwrap().set_controller(ControllerKind::PitchBend, curve).unwrap();
        let out = played.render();
        12.0 * (zero_crossing_hz(&out.left[beats(1.5..2.5)], SR) / 440.0).log2()
    };
    assert!((pitch(&SUB, 1.0) - 2.0).abs() < 0.02, "two semitones where nothing is set");
    assert!((pitch(&wide, 1.0) - 12.0).abs() < 0.02, "an octave up: {}", pitch(&wide, 1.0));
    assert!((pitch(&wide, -1.0) + 12.0).abs() < 0.02 && (pitch(&wide, 0.25) - 3.0).abs() < 0.02);

    // What writes for the track asks the session how far its wheel bends:
    // the instrument's setting, unless the track itself says otherwise.
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
    assert_eq!(session.profile_of(track).bend_semitones(), 2.0, "no instrument: the wheel's two semitones");
    let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
    assert_eq!((session.instrument_bend_range(track), session.profile_of(track).bend_range), (Some(2.0), None));
    session.set_device_params(synth, &[("bend_range".to_string(), 24.0)]).unwrap();
    assert_eq!(session.profile_of(track).bend_semitones(), 24.0);
    assert!(session.project().track(track).unwrap().profile.is_empty(), "the track's own profile says nothing still");
    session
        .edit(|project| {
            project.track_mut(track)?.profile.bend_range = Some(7.0);
            Ok(())
        })
        .unwrap();
    assert_eq!(session.profile_of(track).bend_semitones(), 7.0, "what the track says goes before the instrument");
    assert_eq!(session.profile_of(TrackId(999)).bend_semitones(), 2.0);
}

#[test]
fn noise_plays_alone_and_as_wide_as_it_is_set() {
    // Operator as the song's noise: no oscillator, white noise through a
    // band-pass, a noise of its own on each side.
    let noise = |width: f64, oscillators: f64| {
        let params = [
            ("osc_level", oscillators),
            ("noise_level", 1.0),
            ("noise_width", width),
            ("filter_type", 3.0),
            ("cutoff", 8000.0),
            ("resonance", 0.15),
            ("velocity_sens", 0.0),
            ("volume", -20.0),
        ];
        Played::synth(&params, &[note(60, 0.0, 2.0, 100)], 3.0).render()
    };
    let alike = |out: &Rendered| {
        let (left, right) = (&out.left[beats(0.5..2.0)], &out.right[beats(0.5..2.0)]);
        let dot: f64 = left.iter().zip(right).map(|(l, r)| f64::from(*l) * f64::from(*r)).sum();
        dot / (f64::from(rms(left)) * f64::from(rms(right)) * left.len() as f64)
    };
    let (mono, half, wide) = (noise(0.0, 0.0), noise(0.5, 0.0), noise(1.0, 0.0));
    assert_eq!(mono.left, mono.right, "the same noise on both sides");
    assert!(alike(&wide).abs() < 0.05, "nothing in common between the sides: {}", alike(&wide));
    assert!((alike(&half) - 0.707).abs() < 0.05, "half way: {}", alike(&half));
    // The width moves nothing but the width: each side as loud as it was.
    for out in [&half, &wide] {
        let sides = [&out.left, &out.right].map(|side| db(f64::from(rms(&side[beats(0.5..2.0)]))));
        let was = db(f64::from(rms(&mono.left[beats(0.5..2.0)])));
        assert!((sides[0] - was).abs() < 0.2 && (sides[1] - was).abs() < 0.2, "{sides:?} against {was}");
    }
    // With the oscillators off there is no note under the noise: middle C,
    // which the oscillators would play 30 dB over the noise around it.
    let tone = |out: &Rendered| {
        let second = &out.left[beats(1.0..3.0)];
        let (mut re, mut im) = (0.0, 0.0);
        for (n, sample) in second.iter().enumerate() {
            let angle = std::f64::consts::TAU * 261.626 * n as f64 / SR;
            re += f64::from(*sample) * angle.cos();
            im += f64::from(*sample) * angle.sin();
        }
        db(2.0 * (re * re + im * im).sqrt() / second.len() as f64)
    };
    let with_oscillators = Played::synth(
        &[("noise_level", 1.0), ("filter_type", 3.0), ("cutoff", 8000.0), ("resonance", 0.15), ("volume", -20.0)],
        &[note(60, 0.0, 2.0, 100)],
        3.0,
    )
    .render();
    assert!(tone(&with_oscillators) > tone(&wide) + 30.0, "{} against {}", tone(&with_oscillators), tone(&wide));
    // And with neither there is nothing.
    let silent = Played::synth(&[("osc_level", 0.0)], &[note(60, 0.0, 2.0, 100)], 3.0).render();
    assert_eq!(peak(&silent.left), 0.0);

    // How loud: white noise at a level of 1 is 13 dB under an oscillator
    // of the same peak (noise that reaches as high as a sine is 1.8 dB
    // under it on average, and the synth's open filter takes most of a
    // dB off the very top); in a song saved before it is as high as the
    // oscillator, as it was.
    let plain = [("osc_level", 0.0), ("noise_level", 1.0), ("velocity_sens", 0.0), ("sustain", 1.0), ("volume", -21.0)];
    let played = Played::synth(&plain, &[note(60, 0.0, 2.0, 100)], 3.0);
    let level = |out: &Rendered| db(f64::from(rms(&out.left[beats(0.5..2.0)])));
    let sine = db(0.22 * std::f64::consts::FRAC_1_SQRT_2);
    assert!((level(&played.render()) - (sine - 13.0 - 2.6)).abs() < 0.3, "{}", level(&played.render()));
    // Its volume then, 21 dB further down that scale.
    let old = played.render_this(&played.as_saved_in_format_2());
    assert!((level(&old) - (sine - 21.0 - 2.6)).abs() < 0.3, "{}", level(&old));
}

#[test]
fn a_level_drawn_down_to_its_lowest_value_ends_in_silence() {
    // A synth's volume, drawn from where it plays down to the bottom of
    // its range: the note is still held, and nothing is heard.
    let mut played = Played::synth(&SUB, &[note(57, 0.0, 6.0, 110)], 6.0);
    played.automate("volume", &[(0.0, -10.0), (2.0, -10.0), (3.0, -70.0)]);
    let out = played.render();
    assert!(rms(&out.left[beats(0.5..2.0)]) > 0.1);
    assert_eq!(peak(&out.left[beats(3.2..6.0)]), 0.0, "silence, not 70 dB down");
    // In a song saved before, the bottom of that scale was 48 dB down, and
    // a note drawn there is heard there still.
    let mut played = Played::synth(&SUB, &[note(57, 0.0, 6.0, 110)], 6.0);
    played.automate("volume", &[(0.0, -10.0), (2.0, -10.0), (3.0, -48.0)]);
    let old = played.as_saved_in_format_2();
    let out = played.render_this(&old);
    let (loud, quiet) = (rms(&out.left[beats(0.5..2.0)]), rms(&out.left[beats(3.5..5.5)]));
    assert!((db(f64::from(quiet / loud)) + 38.0).abs() < 0.1, "{}", db(f64::from(quiet / loud)));

    // A sample player's volume is silence at the same place.
    let folder = folder();
    let state = sampler::State::new(hit(&folder, 0.5).as_str()).to_bytes();
    let quiet = Played::new("builtin:sampler", &[("volume", -70.0)], Some(state), &[note(60, 0.0, 1.0, 127)], 2.0);
    assert_eq!(peak(&quiet.render().left), 0.0);
}

#[test]
fn a_synths_volume_moves_in_a_line_where_it_moved_in_steps() {
    // A held sine whose volume an envelope raises by 24 dB over two beats,
    // against the same sine left alone: what is left is the gain.
    let sine = [("osc1_wave", 0.0), ("osc_mix", 0.0), ("sustain", 1.0), ("attack", 0.001), ("unison", 1.0)];
    let held = [note(57, 0.0, 4.0, 100)];
    let rendered = |legacy: f64, moved: bool| {
        let mut params = sine.to_vec();
        params.extend([("volume", -6.0), ("legacy", legacy)]);
        let mut played = Played::synth(&params, &held, 4.0);
        if moved {
            played.automate("volume", &[(0.0, -30.0), (1.0, -30.0), (3.0, -6.0), (4.0, -6.0)]);
        }
        played.render().left
    };
    let still = rendered(0.0, false);
    let peak = still[beats(1.0..3.0)].iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    // The largest change of the gain from one frame to the next, as a
    // share of the gain, where the sine is far enough from zero to tell.
    let largest_step = |out: &[f32]| {
        let mut largest = 0.0f64;
        for frame in beats(1.1..2.9) {
            if still[frame].abs() > 0.5 * peak && still[frame + 1].abs() > 0.5 * peak {
                let (here, next) = (f64::from(out[frame] / still[frame]), f64::from(out[frame + 1] / still[frame + 1]));
                largest = largest.max((next / here - 1.0).abs());
            }
        }
        largest
    };
    let (now, then) = (rendered(0.0, true), rendered(2.0, true));
    let (line, steps) = (largest_step(&now), largest_step(&then));
    println!(
        "the gain's largest step from one frame to the next: {:.4} %, before format 7 {:.2} %",
        line * 100.0,
        steps * 100.0
    );
    // Under an envelope a parameter is told its value every 256 frames. The
    // volume goes from one to the next in a line, as the Utility's gain
    // does; in a song saved before format 7 it jumps there, a step of a
    // percent or two of the level 172 times a second.
    assert!(line < 0.0005, "{line}");
    assert!(steps > 0.005, "{steps}");
    // Both are the same fade: one stretch apart at most.
    for beat in [1.5, 2.0, 2.5] {
        let window = beats(beat - 0.05..beat + 0.05);
        let apart = db(f64::from(rms(&now[window.clone()])) / f64::from(rms(&then[window])));
        assert!(apart.abs() < 0.1, "at beat {beat} the two are {apart:.3} dB apart");
    }
    // A volume that stands still is the same samples either way.
    assert!(rendered(0.0, false) == rendered(2.0, false));
}

// ---- the sampled instrument ----------------------------------------------------
//
// Held to numbers on instruments that are made up here: recordings that
// are tones say at what pitch and how cleanly they come out. Nothing of
// anyone's is played.

/// A song of one track that plays the instrument `sfz` describes, whose
/// recordings (where each is from the instrument's file, its sample rate,
/// its samples) are written into `folder` with it.
fn sampled(
    folder: &tempfile::TempDir,
    sfz: &str,
    recordings: &[(String, f64, Vec<f32>)],
    params: &[(&str, f64)],
    notes: &[Note],
    length: f64,
) -> Played {
    let recordings: Vec<(&str, f64, Vec<f32>)> =
        recordings.iter().map(|(name, rate, samples)| (name.as_str(), *rate, samples.clone())).collect();
    let path = write_instrument(folder.path(), "Made Up", sfz, &recordings);
    let state = trill_dsp::sampled::State::new(path).to_bytes();
    // At the level the instrument was made at, unless the test says.
    let mut all = vec![("volume", 0.0)];
    all.extend(params.iter().copied());
    Played::new(trill_dsp::sampled::URI, &all, Some(state), notes, length)
}

fn key_hz(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

#[test]
fn a_sampled_instrument_plays_every_key_within_a_hundredth_of_a_cent_of_its_pitch() {
    // A recording every third key from C3 to C6, as sampled instruments
    // are made, each a tone at its key's own pitch and at 44.1 kHz: so
    // every note is read at another speed than it was recorded at, and
    // two notes of three at another pitch as well.
    let folder = folder();
    let roots: Vec<u8> = (48..=84).step_by(3).collect();
    let recordings: Vec<(String, f64, Vec<f32>)> =
        roots.iter().map(|root| (format!("samples/{root}.wav"), 44_100.0, sine(key_hz(*root), 44_100.0, 1.5, 0.5))).collect();
    let mut sfz = "<global> amp_veltrack=0 ampeg_release=0.05\n".to_string();
    for root in &roots {
        sfz += &format!("<region> sample=samples/{root}.wav lokey={} hikey={} pitch_keycenter={root}\n", root - 1, root + 1);
    }
    let keys: Vec<u8> = (47..=85).collect();
    let notes: Vec<Note> = keys.iter().enumerate().map(|(index, key)| note(*key, index as f64 * 2.0, 1.5, 100)).collect();
    let out = sampled(&folder, &sfz, &recordings, &[], &notes, keys.len() as f64 * 2.0).render();

    let mut worst = (0.0f64, 0u8);
    for (index, key) in keys.iter().enumerate() {
        let window = beats(index as f64 * 2.0 + 0.2..index as f64 * 2.0 + 1.3);
        let heard = zero_crossing_hz(&out.left[window.clone()], SR);
        let off = 1_200.0 * (heard / key_hz(*key)).log2();
        if off.abs() > worst.0.abs() {
            worst = (off, *key);
        }
        assert!(off.abs() < 0.01, "key {key} is {off:+.4} cents from its pitch ({heard:.3} Hz)");
        assert!((rms(&out.left[window]) - 0.5 / 2f32.sqrt()).abs() < 0.003, "key {key} is not as loud as its recording");
    }
    println!("{} keys from 13 recordings: the worst is {:+.5} cents off (key {})", keys.len(), worst.0, worst.1);
}

/// What is in `played` under `top` Hz that does not belong, in dB under
/// what does: the recording was equal tones `apart` Hz from one another,
/// played at `rate` times its speed, so what belongs lies every
/// `apart * rate` Hz and anything between is a tone that folded back from
/// above.
fn folded_under(played: &[f32], apart: f64, rate: f64, top: f64) -> f64 {
    let size = 1 << 14;
    let spectrum = spectrum_db(played, size);
    let bin = SR / size as f64;
    let spacing = apart * rate;
    let (mut belongs, mut folded) = (f64::MIN, f64::MIN);
    for (index, level) in spectrum.iter().enumerate() {
        let hz = index as f64 * bin;
        let nearest = (hz / spacing).round() * spacing;
        if (hz - nearest).abs() < 6.0 * bin && nearest >= spacing && nearest <= 18_000.0 {
            belongs = belongs.max(f64::from(*level));
        } else if (hz - nearest).abs() > 80.0 && hz > 200.0 && hz < top {
            folded = folded.max(f64::from(*level));
        }
    }
    belongs - folded
}

#[test]
fn a_bright_recording_played_above_its_pitch_folds_seventy_decibels_under_what_belongs() {
    // A recording as bright as one can be: equal tones every 487 Hz up to
    // 23.9 kHz. Played up, the top of it would lie above what the output
    // holds and fold back between the tones that belong. (487, so that
    // what folds does not land on a tone that belongs and hide there, as
    // it would an octave up from tones a round number apart.)
    let folder = folder();
    let tones: Vec<f64> = (1..=49).map(|tone| f64::from(tone) * 487.0).collect();
    let bright: Vec<f32> = (0..1 << 17)
        .map(|frame| {
            let at = f64::from(frame) / SR;
            tones.iter().map(|hz| (std::f64::consts::TAU * hz * at + hz).sin()).sum::<f64>() as f32 / 49.0
        })
        .collect();
    let sfz = "<region> sample=bright.wav pitch_keycenter=60 amp_veltrack=0";
    let recordings = [("bright.wav".to_string(), SR, bright)];
    // Held to seventy decibels under what belongs, up to two octaves
    // above the recording's pitch: both where it is heard, under 20 kHz,
    // and anywhere at all (what folds first lands just under half the
    // sample rate, which nobody hears and a distortion after the
    // instrument would bring down again).
    println!("a recording of tones to 23.9 kHz, played above its pitch: what folds, under what belongs");
    println!("  semitones up   under 20 kHz   anywhere");
    for semitones in [1u8, 3, 5, 7, 10, 12, 19, 24] {
        let played = sampled(&folder, sfz, &recordings, &[], &[note(60 + semitones, 0.0, 4.0, 100)], 4.0).render();
        let rate = 2f64.powf(f64::from(semitones) / 12.0);
        let heard = folded_under(&played.left[2_048..], 487.0, rate, 20_000.0);
        let anywhere = folded_under(&played.left[2_048..], 487.0, rate, 24_000.0);
        println!("  {semitones:>12}   {heard:>9.1} dB   {anywhere:.1} dB");
        assert!(heard > 70.0, "{semitones} semitones up, what folds under 20 kHz is only {heard:.1} dB under what belongs");
        assert!(anywhere > 70.0, "{semitones} semitones up, what folds is only {anywhere:.1} dB under what belongs");
    }
}

#[test]
fn a_sampled_instrument_makes_no_click_at_a_loop_a_release_a_taken_voice_or_a_layer_crossing() {
    let folder = folder();
    // The step from one frame to the next of a tone at full swing: a click
    // is a step several times that.
    let natural = |hz: f64, level: f64| (std::f64::consts::TAU * hz / SR * level) as f32;

    // A loop that the recording's own file names, fifty whole waves long.
    let tone = [("held.wav".to_string(), SR, sine(100.0, SR, 1.0, 0.5))];
    let sfz = "<region> sample=held.wav amp_veltrack=0 ampeg_release=0.1";
    for key in [60u8, 65] {
        let played = sampled(&folder, sfz, &tone, &[], &[note(key, 0.0, 8.0, 100)], 9.0);
        write_loop(&folder.path().join("held.wav"), 9_600, 33_599);
        let out = played.render();
        let rate = 2f32.powf((f32::from(key) - 60.0) / 12.0);
        // (From after the first frames: a recording that begins at once
        // is a corner, which read at another speed rings for a frame or
        // two. The joins of the loop come every half second from there.)
        let step = largest_step(&out.left[200..]);
        assert!(step < natural(100.0, 0.5) * rate * 1.05, "key {key} round its loop: a step of {step}");
        // Held for four seconds from a recording of one.
        assert!((rms(&out.left[beats(6.0..7.9)]) - 0.3535).abs() < 0.003, "key {key} goes on round its loop");
        let heard = zero_crossing_hz(&out.left[beats(1.0..7.9)], SR);
        assert!((1_200.0 * (heard / (100.0 * f64::from(rate))).log2()).abs() < 0.02, "key {key} keeps its pitch");
    }

    // Let go of in the middle of a wave, by an instrument that says nothing of its release.
    let folder = self::folder();
    let long = [("long.wav".to_string(), SR, sine(100.0, SR, 4.0, 0.5))];
    let played = sampled(&folder, "<region> sample=long.wav amp_veltrack=0", &long, &[], &[note(60, 0.0, 1.013, 100)], 4.0);
    let out = played.render();
    let step = largest_step(&out.left);
    assert!(step < natural(100.0, 0.5) * 3.0, "let go of mid-wave: a step of {step}");
    assert_eq!(peak(&out.left[beats(1.2..4.0)]), 0.0, "and gone within forty milliseconds");

    // Two voices allowed and a note every 50 ms, each held: every note takes one.
    let notes: Vec<Note> = (0..30).map(|index| note(55 + (index % 7) as u8, f64::from(index) * 0.1 + 0.013, 2.0, 100)).collect();
    let sfz = "<region> sample=long.wav amp_veltrack=0 volume=-12 ampeg_release=0.3";
    let out = sampled(&folder, sfz, &long, &[("voices", 2.0)], &notes, 5.0).render();
    // Up to three overlap while one goes, each at a quarter of the recording and up to a fifth above it.
    let limit = natural(100.0, 0.5) * 1.5 * 0.25 * 3.0 * 2.0;
    let step = largest_step(&out.left);
    assert!(step < limit, "a voice taken for a new note: a step of {step} (limit {limit})");

    // Two layers that a controller fades between, swept across while a note sounds and then dropped at once.
    let folder = self::folder();
    let layers = [
        ("soft.wav".to_string(), SR, sine(220.0, SR, 6.0, 0.2)),
        ("loud.wav".to_string(), SR, sine(220.0, SR, 6.0, 0.6)),
    ];
    let sfz = "<global> amp_veltrack=0 pitch_keycenter=57\n\
               <region> sample=soft.wav xfout_locc1=20 xfout_hicc1=100\n<region> sample=loud.wav xfin_locc1=20 xfin_hicc1=100";
    let mut played = sampled(&folder, sfz, &layers, &[], &[note(57, 0.0, 10.0, 100)], 10.0);
    let clip = played.project.tracks[0].clips[0].id;
    let wheel = vec![
        AutomationPoint::new(0.0, 0.0),
        AutomationPoint::new(2.0, 0.0),
        AutomationPoint::new(6.0, 1.0),
        AutomationPoint::new(8.0, 1.0),
        AutomationPoint::new(8.0, 0.0),
    ];
    played.project.clip_mut(clip).unwrap().set_controller(ControllerKind::MOD_WHEEL, wheel).unwrap();
    let out = played.render();
    assert!((rms(&out.left[beats(1.0..1.9)]) - 0.2 / 2f32.sqrt()).abs() < 0.002, "the soft layer alone");
    assert!((rms(&out.left[beats(7.0..7.9)]) - 0.6 / 2f32.sqrt()).abs() < 0.002, "the loud layer alone");
    assert!((rms(&out.left[beats(9.0..9.9)]) - 0.2 / 2f32.sqrt()).abs() < 0.002, "and the soft one again");
    // The level itself moves, most where the controller drops at once:
    // a fifth more than the loud layer's own step is the most that adds.
    let step = largest_step(&out.left[..beats(0.0..9.9).end]);
    assert!(step < natural(220.0, 0.6) * 1.2, "across the crossing and back: a step of {step}");
}

/// Plays `project` from the top for `frames` frames in buffers of `block`
/// through devices that are made to be rendered: the audio thread's own
/// code, at a buffer size a render does not use.
fn played_in_buffers(project: &Project, factory: &Builtins, frames: usize, block: usize) -> Vec<f32> {
    use trill_engine::compile::{Compiler, DeviceBank, Meters};
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), trill_engine::AudioSources::default());
    let (graph, problems) = Compiler {
        factory,
        bank: &mut bank,
        meters: &mut meters,
        audio: &audio,
        stand_ins: None,
        sample_rate: SR,
        purpose: trill_core::device::Purpose::Rendering,
    }
    .compile(project, true);
    assert!(problems.is_empty(), "{problems:?}");
    let mut processor = trill_engine::Processor::new(SR);
    drop(processor.swap_graph(graph));
    processor.seek(0.0);
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        processor.process(l, r);
    }
    left
}

#[test]
fn a_song_of_sampled_instruments_is_the_same_samples_at_any_buffer_size_and_on_any_number_of_threads() {
    let folder = folder();
    let dir = folder.path();
    // Hits that are taken in turn and at random; layers a controller fades
    // between, with a vibrato and a loop; and long recordings held by the
    // pedal, with a sound of their own for a key coming up.
    let hit = |hz: f64| -> Vec<f32> {
        sine(hz, 44_100.0, 0.25, 0.5).iter().enumerate().map(|(n, s)| s * (-(n as f32) / 2_500.0).exp()).collect()
    };
    let kit = write_instrument(
        dir,
        "Kit",
        "<global> loop_mode=one_shot amp_random=3 pitch_random=30 offset_random=40\n\
         <group> key=36 seq_length=3\n<region> sample=k/a.wav seq_position=1\n<region> sample=k/b.wav seq_position=2\n\
         <region> sample=k/c.wav seq_position=3\n\
         <group> key=38\n<region> sample=k/a.wav lorand=0 hirand=0.3\n<region> sample=k/b.wav lorand=0.3 hirand=0.7\n\
         <region> sample=k/c.wav lorand=0.7 hirand=1\n<group> key=42 group=1 off_by=1 <region> sample=k/c.wav delay_random=0.004",
        &[("k/a.wav", 44_100.0, hit(180.0)), ("k/b.wav", 44_100.0, hit(240.0)), ("k/c.wav", 44_100.0, hit(900.0))],
    );
    let strings = write_instrument(
        dir,
        "Strings",
        "<global> ampeg_attack=0.05 ampeg_release=0.2 loop_mode=loop_sustain loop_start=10000 loop_end=30000 loop_crossfade=0.05\n\
         pitchlfo_freq=5.5 pitchlfo_depth_oncc1=25 cutoff=3000 cutoff_cc1=2400 lokey=40 hikey=80\n\
         <region> sample=s/p.wav pitch_keycenter=55 xfout_locc1=30 xfout_hicc1=110\n\
         <region> sample=s/f.wav pitch_keycenter=55 xfin_locc1=30 xfin_hicc1=110",
        &[("s/p.wav", 48_000.0, sine(196.0, 48_000.0, 1.0, 0.2)), ("s/f.wav", 48_000.0, sine(196.3, 48_000.0, 1.0, 0.4))],
    );
    let piano = write_instrument(
        dir,
        "Piano",
        "<global> ampeg_release=0.4\n<group> lovel=1 hivel=90\n<region> sample=p/c3.wav lokey=36 hikey=54 pitch_keycenter=48\n\
         <region> sample=p/c5.wav lokey=55 hikey=96 pitch_keycenter=72\n\
         <group> lovel=91 hivel=127 volume=3\n<region> sample=p/c3.wav lokey=36 hikey=54 pitch_keycenter=48 tune=3\n\
         <region> sample=p/c5.wav lokey=55 hikey=96 pitch_keycenter=72 tune=-2\n\
         <group> trigger=release rt_decay=4 volume=-14 <region> sample=k/c.wav lokey=36 hikey=96 pitch_keytrack=0",
        &[("p/c3.wav", 44_100.0, sine(130.8, 44_100.0, 3.0, 0.3)), ("p/c5.wav", 44_100.0, sine(523.3, 44_100.0, 3.0, 0.3))],
    );

    let mut project = Project::new("three");
    project.master.devices.clear();
    project.set_tempo(TEMPO).unwrap();
    let mut part = |name: &str, instrument: &str, notes: &[Note], curves: &[(ControllerKind, Vec<(f64, f64)>)]| {
        let track = project.add_track(name);
        let device = project.add_device(ChainRef::Track(track), trill_dsp::sampled::URI, name, None).unwrap();
        project.device_mut(device).unwrap().state = Some(trill_dsp::sampled::State::new(instrument).to_bytes());
        let clip = project.add_midi_clip(track, "part", 0.0, 16.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
        for (kind, points) in curves {
            let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
            project.clip_mut(clip).unwrap().set_controller(*kind, points).unwrap();
        }
    };
    let beat: Vec<Note> = (0..64)
        .flat_map(|step| {
            let at = f64::from(step) * 0.25 + f64::from(step % 5) * 0.011;
            [note(36, at, 0.2, 100), note(38, at + 0.125, 0.2, 80 + (step % 40) as u8), note(42, at, 0.1, 90)]
        })
        .collect();
    part("Kit", &kit, &beat, &[]);
    let chords: Vec<Note> =
        (0..4).flat_map(|bar| [55u8, 59, 62, 67].map(|key| note(key + bar as u8, f64::from(bar) * 4.0, 3.7, 100))).collect();
    let wheel = vec![(0.0, 0.0), (3.0, 1.0), (5.0, 0.2), (9.0, 0.9), (16.0, 0.0)];
    part("Strings", &strings, &chords, &[(ControllerKind::MOD_WHEEL, wheel), (ControllerKind::PitchBend, vec![(0.0, 0.0), (8.0, 0.0), (10.0, 0.6), (12.0, 0.0)])]);
    let tune: Vec<Note> = (0..40).map(|step| note(40 + (step * 7 % 50) as u8, f64::from(step) * 0.4, 0.35, 50 + (step * 13 % 78) as u8)).collect();
    let pedal = vec![(0.0, 0.0), (1.0, 1.0), (3.9, 1.0), (4.0, 0.0), (4.2, 1.0), (7.9, 1.0), (8.0, 0.0), (12.0, 1.0), (15.5, 0.0)];
    part("Piano", &piano, &tune, &[(ControllerKind::SUSTAIN, pedal)]);

    let factory = Builtins::with_loader(Arc::new(MediaCache::default()));
    let options = |threads: Option<usize>| RenderOptions { sample_rate: SR as u32, max_tail_seconds: 1.0, threads, ..RenderOptions::default() };
    let one = render(&project, &factory, &options(Some(1))).unwrap();
    assert!(one.problems.is_empty(), "{:?}", one.problems);
    assert!(rms(&one.left) > 0.05 && one.left.len() > 8 * SR as usize, "the song sounds");
    // Not close: the same. In a second render, and on any number of threads.
    for threads in [Some(1), None, Some(2), Some(3), Some(64)] {
        let again = render(&project, &factory, &options(threads)).unwrap();
        assert!(again.left == one.left && again.right == one.right, "{threads:?} threads: other samples");
    }
    // And played in buffers of any size by the audio thread's own code.
    let frames = 8 * SR as usize;
    for block in [512, 480, 441, 37, 1_024, 4_096] {
        let played = played_in_buffers(&project, &factory, frames, block);
        assert!(played[..] == one.left[..frames], "buffers of {block} frames: other samples");
    }
}

#[test]
fn a_hundred_and_twenty_eight_voices_of_a_sampled_instrument_cost_less_than_the_time_they_play() {
    // Every key held for ten seconds, each a voice that goes round a loop
    // of a recording made at another rate than the song's: so every frame
    // of every voice is worked out from thirty-two or more stored ones.
    let folder = folder();
    let recordings: Vec<(String, f64, Vec<f32>)> = [36u8, 60, 84]
        .iter()
        .map(|root| (format!("{root}.wav"), 44_100.0, sine(key_hz(*root), 44_100.0, 2.0, 0.05)))
        .collect();
    let sfz = "<global> amp_veltrack=0 loop_mode=loop_continuous loop_start=20000 loop_end=80000 ampeg_release=0.1\n\
               <region> sample=36.wav lokey=0 hikey=47 pitch_keycenter=36 pitch_keytrack=5\n\
               <region> sample=60.wav lokey=48 hikey=71 pitch_keycenter=60 pitch_keytrack=5\n\
               <region> sample=84.wav lokey=72 hikey=127 pitch_keycenter=84 pitch_keytrack=5";
    let notes: Vec<Note> = (0..128u8).map(|key| note(key, 0.0, 20.0, 100)).collect();
    let played = sampled(&folder, sfz, &recordings, &[("voices", 128.0)], &notes, 20.0);
    let _warm = played.render();
    let begun = std::time::Instant::now();
    let out = played.render();
    let took = begun.elapsed().as_secs_f64();
    let seconds = out.left.len() as f64 / SR;
    assert!(rms(&out.left[beats(10.0..19.0)]) > 0.2, "all of them sound: {}", rms(&out.left[beats(10.0..19.0)]));
    let share = took / seconds;
    println!(
        "128 voices for {seconds:.1} s took {took:.2} s: {:.1} % of one processor, {:.3} % a voice",
        share * 100.0,
        share * 100.0 / 128.0
    );
    assert!(share < 1.0, "128 voices took {took:.2} s to play {seconds:.1} s");
}
