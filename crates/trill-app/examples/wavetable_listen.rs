//! Writes what the wavetable synth's presets sound like, for someone to
//! listen to:
//!
//! ```sh
//! cargo run -p trill-app --example wavetable_listen -- target/wavetable-listening
//! ```
//!
//! Every preset plays a phrase that suits it, alone and as it is (no
//! effect, no limiter), into a WAV file; the ones that are usually heard
//! through effects are written a second time through Trill's own chorus,
//! delay and reverb. Two short pieces use several of them together, mixed
//! as a song is. `LIST.md` says which file is which.
//!
//! Nobody has heard any of this: whoever wrote the presets and the phrases
//! cannot. It is the same on every run and on every machine.
//!
//! With a second path it also writes the small song the pictures of the
//! synth's panel are made from (`render.sh wavetable`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use trill_core::audio::AudioBuffer;
use trill_core::device::DeviceFactory;
use trill_core::project::{AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};
use trill_media::{WavFormat, write_wav};

const SYNTH: &str = "builtin:wavetable";

fn n(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

/// What a preset of the synth sets, and the state it brings.
fn preset(name: &str) -> (BTreeMap<String, f64>, Option<Vec<u8>>, String, String) {
    let descriptors = BuiltinFactory.descriptors();
    let synth = descriptors.iter().find(|descriptor| descriptor.uri == SYNTH).expect("the wavetable synth");
    let preset = synth.presets.iter().find(|preset| preset.name == name).unwrap_or_else(|| panic!("no '{name}'"));
    let values = preset.params.iter().cloned().collect();
    (values, preset.state.clone(), preset.category.clone(), preset.description.clone())
}

/// Adds a track that plays `notes` on the synth's preset `name`, in a clip
/// from the start of the song. Returns the track and the synth.
fn part(project: &mut Project, name: &str, preset_name: &str, notes: &[Note], beats: f64) -> (TrackId, DeviceId) {
    let track = project.add_track(name);
    let device = project.add_device(ChainRef::Track(track), SYNTH, preset_name, None).expect("a device");
    let (values, state, ..) = preset(preset_name);
    let model = project.device_mut(device).expect("the device");
    model.params = values;
    model.state = state;
    let clip = project.add_midi_clip(track, name, 0.0, beats).expect("a clip");
    project.clip_mut(clip).expect("the clip").add_notes(notes).expect("notes");
    (track, device)
}

fn effect(project: &mut Project, chain: ChainRef, uri: &str, values: &[(&str, f64)]) {
    let device = project.add_device(chain, uri, uri.trim_start_matches("builtin:"), None).expect("an effect");
    let model = project.device_mut(device).expect("the effect");
    model.params.extend(values.iter().map(|(id, value)| (id.to_string(), *value)));
}

fn draw(project: &mut Project, device: DeviceId, param: &str, points: &[(f64, f64)]) {
    let target = AutomationTarget::DeviceParam { device, param: param.to_string() };
    let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
    project.set_automation(target, points).expect("automation");
}

/// The highest a file goes: one that would go higher is turned down to
/// it, since a file cannot hold more than full scale.
const CEILING: f32 = 0.891;

/// Renders `project` and writes it to `path`. Returns its length in
/// seconds and its peak as it was rendered, in dBFS, as words: with by
/// how much the file was turned down, if it had to be.
fn write(project: &Project, path: &Path) -> anyhow::Result<(f64, String)> {
    let options = RenderOptions { sample_rate: 48_000, max_tail_seconds: 5.0, ..RenderOptions::default() };
    let mut out = render(project, &BuiltinFactory, &options).map_err(|error| anyhow::anyhow!("{error}"))?;
    anyhow::ensure!(out.problems.is_empty(), "{:?}", out.problems);
    let peak = out.left.iter().chain(&out.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let seconds = out.left.len() as f64 / 48_000.0;
    let db = |gain: f32| 20.0 * f64::from(gain.max(1e-9)).log10();
    let mut said = format!("{:.1} dBFS", db(peak));
    if peak > CEILING {
        let down = CEILING / peak;
        for sample in out.left.iter_mut().chain(out.right.iter_mut()) {
            *sample *= down;
        }
        said = format!("{said} as rendered; the file is {:.1} dB down from that, to fit", -db(down));
    }
    let buffer = AudioBuffer { sample_rate: 48_000.0, left: out.left, right: out.right };
    write_wav(path, &buffer, WavFormat::Int24).map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok((seconds, said))
}

/// Triads and sevenths by name, as the notes of one voicing.
fn chord(name: &str) -> &'static [u8] {
    match name {
        "Em" => &[52, 55, 59, 64],
        "C" => &[48, 52, 55, 60, 64],
        "G" => &[43, 50, 55, 59],
        "D" => &[50, 54, 57, 62],
        "Am" => &[45, 52, 57, 60],
        "F" => &[41, 48, 53, 57],
        "Em9" => &[52, 59, 62, 67, 66],
        "Cmaj7" => &[48, 55, 59, 64],
        "Dm9" => &[50, 57, 60, 64, 65],
        "G13" => &[43, 53, 59, 64],
        "Cmaj9" => &[48, 55, 59, 62, 64],
        "Am9" => &[45, 55, 59, 60, 64],
        _ => &[60, 64, 67],
    }
}

/// `names` held for `each` beats one after another, every note of a chord
/// at `velocity` (a little softer the higher it is).
fn held(names: &[&str], each: f64, velocity: u8) -> Vec<Note> {
    names
        .iter()
        .enumerate()
        .flat_map(|(index, name)| {
            let start = index as f64 * each;
            chord(name)
                .iter()
                .enumerate()
                .map(move |(voice, pitch)| n(*pitch, start, each - 0.1, velocity.saturating_sub(voice as u8 * 3)))
        })
        .collect()
}

/// An arpeggio up and down each chord of `names`, `steps` notes of
/// `length` beats to the chord, `octave` semitones up.
fn arpeggio(names: &[&str], steps: usize, length: f64, octave: u8) -> Vec<Note> {
    let mut notes = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let tones = chord(name);
        let up_and_down: Vec<u8> =
            tones.iter().chain(tones.iter().rev().skip(1).take(tones.len().saturating_sub(2))).copied().collect();
        for step in 0..steps {
            let pitch = up_and_down[step % up_and_down.len()] + octave;
            let velocity = [112, 78, 92, 70][step % 4] + if step % 8 == 0 { 8 } else { 0 };
            notes.push(n(pitch, index as f64 * steps as f64 * length + step as f64 * length, length * 0.9, velocity));
        }
    }
    notes
}

/// The phrase a preset plays alone: its tempo, its notes, how many beats,
/// what is drawn over it, and what it plays in words.
struct Phrase {
    tempo: f64,
    notes: Vec<Note>,
    beats: f64,
    drawn: Vec<(&'static str, Vec<(f64, f64)>)>,
    plays: &'static str,
}

fn phrase(preset: &str) -> Phrase {
    let line = |notes: &[(u8, f64, f64, u8)]| notes.iter().map(|(p, s, l, v)| n(*p, *s, *l, *v)).collect::<Vec<_>>();
    let twice = |notes: Vec<Note>, bars: f64| {
        let mut all = notes.clone();
        all.extend(notes.iter().map(|note| Note { start: note.start + bars, ..*note }));
        all
    };
    match preset {
        "Solid Bass" => Phrase {
            tempo: 120.0,
            notes: twice(
                line(&[
                    (40, 0.0, 0.45, 110),
                    (40, 0.5, 0.2, 80),
                    (43, 1.0, 0.45, 100),
                    (40, 1.5, 0.2, 85),
                    (45, 2.0, 0.45, 110),
                    (40, 2.5, 0.2, 80),
                    (47, 3.0, 0.45, 105),
                    (50, 3.5, 0.4, 95),
                    (40, 4.0, 0.9, 115),
                    (52, 5.0, 0.2, 90),
                    (50, 5.5, 0.2, 85),
                    (47, 6.0, 0.45, 100),
                    (45, 6.5, 0.45, 95),
                    (43, 7.0, 0.9, 105),
                ]),
                8.0,
            ),
            beats: 16.0,
            drawn: vec![("macro1", vec![(0.0, 0.0), (8.0, 0.0), (16.0, 0.7)])],
            plays: "a bass line in E minor, twice; the second time the Cutoff macro opens",
        },
        "Reese" => Phrase {
            tempo: 120.0,
            notes: line(&[
                (28, 0.0, 4.0, 105),
                (31, 3.9, 2.1, 100),
                (33, 5.9, 2.1, 100),
                (28, 8.0, 3.0, 110),
                (26, 10.9, 1.1, 100),
                (28, 12.0, 4.0, 105),
            ]),
            beats: 16.0,
            drawn: vec![("macro1", vec![(0.0, 0.0), (8.0, 0.1), (16.0, 0.6)])],
            plays: "long low notes that slide into one another, the Cutoff macro opening in the second half",
        },
        "Acid Line" => {
            let pitches = [40u8, 40, 52, 40, 43, 40, 52, 47, 40, 40, 50, 40, 45, 52, 40, 47];
            let notes = (0..64)
                .map(|step| {
                    let (accent, slide) = (step % 16 == 6 || step % 8 == 3, step % 16 == 2 || step % 16 == 13);
                    let length = if slide { 0.32 } else { 0.2 };
                    n(pitches[step % 16], step as f64 * 0.25, length, if accent { 127 } else { 84 })
                })
                .collect();
            Phrase {
                tempo: 126.0,
                notes,
                beats: 16.0,
                drawn: vec![("macro1", vec![(0.0, 0.0), (16.0, 0.8)])],
                plays: "sixteenths with accents and slides, the Squelch macro rising over four bars",
            }
        }
        "Growl Bass" => Phrase {
            tempo: 140.0,
            notes: line(&[
                (28, 0.0, 3.9, 110),
                (31, 4.0, 1.9, 105),
                (30, 6.0, 1.9, 105),
                (28, 8.0, 3.9, 110),
                (35, 12.0, 1.9, 105),
                (33, 14.0, 1.9, 105),
            ]),
            beats: 16.0,
            drawn: vec![
                ("macro1", vec![(0.0, 0.0), (8.0, 0.5), (16.0, 1.0)]),
                ("macro4", vec![(0.0, 0.0), (16.0, 0.6)]),
            ],
            plays: "held notes at 140, the Wobble and Vowel macros rising",
        },
        "Deep Sub" => Phrase {
            tempo: 140.0,
            notes: twice(
                line(&[
                    (28, 0.0, 1.5, 115),
                    (28, 2.5, 0.5, 100),
                    (31, 3.0, 1.0, 105),
                    (28, 4.0, 1.5, 115),
                    (26, 6.5, 0.5, 100),
                    (28, 7.0, 1.0, 110),
                ]),
                8.0,
            ),
            beats: 16.0,
            drawn: vec![("macro1", vec![(0.0, 0.0), (8.0, 0.0), (8.01, 0.5), (16.0, 0.5)])],
            plays: "an 808 pattern, twice; the second time with the Tone macro half up",
        },
        "Sync Lead" => Phrase {
            tempo: 120.0,
            notes: line(&[
                (64, 0.0, 1.05, 105),
                (67, 1.0, 0.55, 95),
                (69, 1.5, 0.55, 95),
                (71, 2.0, 1.4, 110),
                (74, 3.5, 0.55, 100),
                (76, 4.0, 1.9, 115),
                (74, 6.0, 0.55, 95),
                (71, 6.5, 0.55, 95),
                (69, 7.0, 0.9, 100),
                (67, 8.0, 1.05, 100),
                (69, 9.0, 0.55, 95),
                (71, 9.5, 0.55, 100),
                (76, 10.0, 3.5, 115),
                (79, 14.0, 0.5, 105),
                (76, 14.5, 1.5, 110),
            ]),
            beats: 16.0,
            drawn: vec![("macro1", vec![(0.0, 0.0), (8.0, 0.0), (16.0, 0.6)])],
            plays: "a melody in E minor with a long note for the vibrato, the Rip macro rising in the second half",
        },
        "Anthem Saws" => Phrase {
            tempo: 128.0,
            notes: twice(held(&["Em", "C", "G", "D"], 4.0, 100), 16.0),
            beats: 32.0,
            drawn: vec![
                ("macro1", vec![(0.0, 0.0), (16.0, 0.0), (32.0, 1.0)]),
                ("macro2", vec![(0.0, 0.0), (16.0, 0.0), (32.0, 0.8)]),
            ],
            plays: "Em, C, G, D, a bar each, twice; the second time the Bright and Octave macros rise",
        },
        "Talk Lead" => Phrase {
            tempo: 110.0,
            notes: line(&[
                (52, 0.0, 2.0, 105),
                (55, 1.95, 1.0, 100),
                (57, 2.95, 1.0, 100),
                (59, 4.0, 2.9, 110),
                (57, 7.0, 0.9, 100),
                (55, 8.0, 2.0, 105),
                (52, 10.0, 5.5, 105),
            ]),
            beats: 16.0,
            drawn: vec![("macro1", vec![(0.0, 0.0), (4.0, 1.0), (8.0, 0.0), (12.0, 1.0), (16.0, 0.2)])],
            plays: "a slow line, the Vowel macro going from 'oo' to 'ee' and back twice",
        },
        "Glass Pad" => Phrase {
            tempo: 90.0,
            notes: held(&["Em9", "Cmaj7"], 8.0, 85),
            beats: 16.0,
            drawn: vec![],
            plays: "Em9 and Cmaj7, two bars each",
        },
        "PWM Strings" => Phrase {
            tempo: 90.0,
            notes: {
                let mut notes = held(&["Am", "F"], 8.0, 90);
                notes.extend(line(&[(72, 2.0, 2.0, 80), (76, 4.0, 3.9, 85), (72, 10.0, 2.0, 80), (69, 12.0, 3.9, 85)]));
                notes
            },
            beats: 16.0,
            drawn: vec![],
            plays: "A minor and F, two bars each, with a line on top",
        },
        "Choir" => Phrase {
            tempo: 80.0,
            notes: held(&["C", "F"], 8.0, 85),
            beats: 16.0,
            drawn: vec![("macro1", vec![(0.0, 0.0), (16.0, 1.0)])],
            plays: "C and F, two bars each, the Vowel macro going from 'ah' round to 'oo'",
        },
        "Glass Pluck" => Phrase {
            tempo: 120.0,
            notes: arpeggio(&["Em", "C", "G", "D"], 16, 0.25, 12),
            beats: 16.0,
            drawn: vec![],
            plays: "sixteenth arpeggios over Em, C, G, D, with accents",
        },
        "String Pluck" => Phrase {
            tempo: 100.0,
            notes: arpeggio(&["Am", "F", "C", "G"], 8, 0.5, 0),
            beats: 16.0,
            drawn: vec![],
            plays: "picked eighths over Am, F, C, G",
        },
        "Trance Pluck" => {
            let pattern = [0.0, 0.75, 1.5, 2.5, 3.25];
            let notes = ["Em", "C", "G", "D"]
                .iter()
                .enumerate()
                .flat_map(|(bar, name)| {
                    pattern.iter().flat_map(move |at| {
                        chord(name).iter().take(4).map(move |pitch| n(*pitch + 12, bar as f64 * 4.0 + at, 0.3, 100))
                    })
                })
                .collect::<Vec<_>>();
            Phrase {
                tempo: 132.0,
                notes: twice(notes, 16.0),
                beats: 32.0,
                drawn: vec![("macro1", vec![(0.0, 0.0), (16.0, 0.0), (32.0, 0.9)])],
                plays: "chord stabs over Em, C, G, D, twice; the second time the Cutoff macro opens",
            }
        }
        "Tine Piano" | "FM Keys" => {
            let mut notes = Vec::new();
            for (bar, (name, velocity)) in [("Dm9", 62u8), ("G13", 84), ("Cmaj9", 70), ("Am9", 100)].iter().enumerate()
            {
                let start = bar as f64 * 4.0;
                notes.extend(chord(name).iter().map(|pitch| n(*pitch, start, 2.4, *velocity)));
                notes.extend(
                    chord(name).iter().skip(1).map(|pitch| n(*pitch, start + 2.5, 1.2, velocity.saturating_sub(12))),
                );
            }
            // A few notes on top, soft and hard, to hear what touch does.
            notes.extend(line(&[
                (76, 1.0, 0.9, 50),
                (77, 5.0, 0.9, 80),
                (79, 9.0, 0.9, 105),
                (81, 13.0, 0.5, 125),
                (79, 13.5, 2.0, 70),
            ]));
            Phrase {
                tempo: 84.0,
                notes,
                beats: 16.0,
                drawn: vec![],
                plays: "Dm9, G13, Cmaj9, Am9, each struck harder than the last, with single notes soft to hard on top",
            }
        }
        _ => Phrase { tempo: 120.0, notes: held(&["Em"], 4.0, 100), beats: 4.0, drawn: vec![], plays: "E minor" },
    }
}

/// The effects a kind of sound is usually heard through.
fn finish(project: &mut Project, track: TrackId, category: &str) {
    let chain = ChainRef::Track(track);
    match category {
        "Lead" => {
            effect(project, chain, "builtin:delay", &[("time_beats", 0.75), ("feedback", 0.35), ("mix", 0.18)]);
            effect(project, chain, "builtin:reverb", &[("decay", 1.8), ("mix", 0.18)]);
        }
        "Pad" => {
            effect(project, chain, "builtin:chorus", &[]);
            effect(project, chain, "builtin:reverb", &[("decay", 4.0), ("mix", 0.3)]);
        }
        "Pluck" => {
            effect(project, chain, "builtin:delay", &[("time_beats", 0.75), ("feedback", 0.4), ("mix", 0.22)]);
            effect(project, chain, "builtin:reverb", &[("decay", 2.2), ("mix", 0.2)]);
        }
        "Keys" => {
            effect(project, chain, "builtin:chorus", &[]);
            effect(project, chain, "builtin:reverb", &[("decay", 1.6), ("mix", 0.16)]);
        }
        _ => {}
    }
}

/// One preset alone on its phrase.
fn alone(name: &str, with_effects: bool) -> Project {
    let phrase = phrase(name);
    let mut project = Project::new(name);
    // As it is: nothing after it, the master's limiter included.
    project.master.devices.clear();
    project.set_tempo(phrase.tempo).expect("a tempo");
    let (track, device) = part(&mut project, name, name, &phrase.notes, phrase.beats);
    for (param, points) in &phrase.drawn {
        draw(&mut project, device, param, points);
    }
    if with_effects {
        let (.., category, _) = preset(name);
        finish(&mut project, track, &category);
    }
    project
}

fn drums(project: &mut Project, from_bar: usize, bars: usize) -> TrackId {
    let track = project.add_track("Drums");
    project.add_device(ChainRef::Track(track), "builtin:drumkit", "Drum Kit", None).expect("drums");
    let mut notes = Vec::new();
    for bar in from_bar..bars {
        let start = bar as f64 * 4.0;
        for beat in 0..4 {
            notes.push(n(36, start + beat as f64, 0.25, 118));
            notes.push(n(42, start + beat as f64 + 0.5, 0.2, 96));
            notes.push(n(42, start + beat as f64 + 0.25, 0.1, 48));
            notes.push(n(42, start + beat as f64 + 0.75, 0.1, 54));
        }
        notes.push(n(39, start + 1.0, 0.25, 104));
        notes.push(n(39, start + 3.0, 0.25, 108));
        notes.push(n(46, start + 3.5, 0.4, 84));
    }
    let clip = project.add_midi_clip(track, "Beat", 0.0, bars as f64 * 4.0).expect("a clip");
    project.clip_mut(clip).expect("the clip").add_notes(&notes).expect("notes");
    track
}

/// A room every part can be sent to.
fn room(project: &mut Project, decay: f64) -> TrackId {
    let room = project.add_track_of_kind("Room", TrackKind::Return);
    effect(project, ChainRef::Track(room), "builtin:reverb", &[("decay", decay), ("mix", 1.0)]);
    room
}

fn mix(project: &mut Project, track: TrackId, volume: f32, to_room: Option<(TrackId, f32)>) {
    project.set_track_volume(track, volume).expect("a volume");
    if let Some((room, level)) = to_room {
        project.set_send(track, room, Some(level)).expect("a send");
    }
}

/// Sixteen bars at 124: a pluck and a pad begin, the bass and the drums
/// come in, chords of stacked saws open over the second half, a lead on
/// top of it.
fn night_drive() -> Project {
    let mut project = Project::new("Night Drive");
    project.set_tempo(124.0).expect("a tempo");
    let bars = ["Em", "C", "G", "D"];
    let song: Vec<&str> = bars.iter().cycle().take(16).copied().collect();
    let room = room(&mut project, 2.6);

    let (pad, _) = part(&mut project, "Pad", "Glass Pad", &held(&["Em9", "Cmaj7", "Em9", "Cmaj7"], 16.0, 80), 64.0);
    mix(&mut project, pad, -13.0, Some((room, -8.0)));
    let (arp, pluck) = part(&mut project, "Arp", "Glass Pluck", &arpeggio(&song, 16, 0.25, 12), 64.0);
    effect(
        &mut project,
        ChainRef::Track(arp),
        "builtin:delay",
        &[("time_beats", 0.75), ("feedback", 0.4), ("mix", 0.2)],
    );
    draw(&mut project, pluck, "macro1", &[(0.0, 0.0), (32.0, 0.2), (64.0, 0.7)]);
    mix(&mut project, arp, -11.0, Some((room, -10.0)));

    // The bass: the root of each bar's chord on the off-beats, from bar 5.
    let roots = [40u8, 36, 43, 38];
    let bass: Vec<Note> = (4..16)
        .flat_map(|bar| {
            (0..4).flat_map(move |beat| {
                let start = bar as f64 * 4.0 + beat as f64;
                [n(roots[bar % 4], start + 0.5, 0.4, 108), n(roots[bar % 4] + 12, start + 0.75, 0.2, 84)]
            })
        })
        .collect();
    let (bass, _) = part(&mut project, "Bass", "Solid Bass", &bass, 64.0);
    mix(&mut project, bass, -6.0, None);
    let beat = drums(&mut project, 4, 16);
    mix(&mut project, beat, -5.0, None);

    let stabs: Vec<Note> = held(&song, 4.0, 96).into_iter().filter(|note| note.start >= 32.0).collect();
    let (chords, saws) = part(&mut project, "Chords", "Anthem Saws", &stabs, 64.0);
    draw(&mut project, saws, "macro1", &[(32.0, 0.0), (64.0, 1.0)]);
    mix(&mut project, chords, -10.0, Some((room, -9.0)));

    let melody = [
        (76u8, 0.0, 1.5, 108u8),
        (74, 1.5, 0.5, 96),
        (71, 2.0, 2.0, 104),
        (72, 4.0, 1.0, 100),
        (76, 5.0, 1.0, 104),
        (79, 6.0, 2.0, 112),
        (78, 8.0, 1.5, 104),
        (74, 9.5, 0.5, 96),
        (71, 10.0, 2.0, 104),
        (69, 12.0, 1.0, 100),
        (71, 13.0, 1.0, 100),
        (76, 14.0, 2.0, 112),
    ];
    let lead: Vec<Note> = melody.iter().map(|(p, s, l, v)| n(*p, 48.0 + s, *l + 0.05, *v)).collect();
    let (lead, _) = part(&mut project, "Lead", "Sync Lead", &lead, 64.0);
    effect(
        &mut project,
        ChainRef::Track(lead),
        "builtin:delay",
        &[("time_beats", 0.75), ("feedback", 0.3), ("mix", 0.16)],
    );
    mix(&mut project, lead, -9.0, Some((room, -9.0)));
    project
}

/// Twelve bars at 84: an electric piano, strings under it, a sub, a
/// plucked string with the tune, and a choir that comes in half way and
/// changes its vowel.
fn slow_glass() -> Project {
    let mut project = Project::new("Slow Glass");
    project.set_tempo(84.0).expect("a tempo");
    let song: Vec<&str> = ["Dm9", "G13", "Cmaj9", "Am9"].iter().cycle().take(12).copied().collect();
    let room = room(&mut project, 3.2);

    let mut comping = Vec::new();
    for (bar, name) in song.iter().enumerate() {
        let start = bar as f64 * 4.0;
        comping.extend(chord(name).iter().map(|pitch| n(*pitch, start, 2.3, 78)));
        comping.extend(chord(name).iter().skip(1).map(|pitch| n(*pitch, start + 2.5, 1.3, 64)));
    }
    let (keys, _) = part(&mut project, "Keys", "Tine Piano", &comping, 48.0);
    mix(&mut project, keys, -7.0, Some((room, -12.0)));
    let (strings, _) = part(&mut project, "Strings", "PWM Strings", &held(&song, 4.0, 72), 48.0);
    mix(&mut project, strings, -13.0, Some((room, -8.0)));
    let roots = [38u8, 31, 36, 33];
    let low: Vec<Note> = (0..12).map(|bar| n(roots[bar % 4], bar as f64 * 4.0, 3.6, 100)).collect();
    let (sub, _) = part(&mut project, "Sub", "Deep Sub", &low, 48.0);
    mix(&mut project, sub, -8.0, None);
    let tune = [
        (69u8, 0.5, 0.5, 96u8),
        (72, 1.0, 0.5, 88),
        (76, 1.5, 1.0, 104),
        (74, 2.5, 0.5, 90),
        (72, 3.0, 1.0, 96),
        (71, 4.5, 0.5, 92),
        (74, 5.0, 0.5, 88),
        (77, 5.5, 1.0, 104),
        (76, 6.5, 0.5, 90),
        (74, 7.0, 1.0, 96),
        (72, 8.5, 0.5, 92),
        (76, 9.0, 0.5, 96),
        (79, 9.5, 1.5, 108),
        (76, 11.0, 1.0, 92),
        (72, 12.5, 0.5, 90),
        (71, 13.0, 0.5, 88),
        (69, 13.5, 2.0, 100),
    ];
    let picked: Vec<Note> =
        (0..3).flat_map(|pass| tune.iter().map(move |(p, s, l, v)| n(*p, pass as f64 * 16.0 + s, *l, *v))).collect();
    let (pluck, _) = part(&mut project, "String", "String Pluck", &picked, 48.0);
    mix(&mut project, pluck, -6.0, Some((room, -8.0)));
    let voices: Vec<Note> = held(&song, 4.0, 76).into_iter().filter(|note| note.start >= 16.0).collect();
    let (choir, singers) = part(&mut project, "Choir", "Choir", &voices, 48.0);
    draw(&mut project, singers, "macro1", &[(16.0, 0.0), (32.0, 0.5), (48.0, 1.0)]);
    mix(&mut project, choir, -13.0, Some((room, -6.0)));
    project
}

/// The song the pictures of the panel are made from: one preset with a
/// table of its own on the second oscillator, playing a few notes.
fn for_pictures() -> Project {
    let mut project = alone("Sync Lead", false);
    project.name = "Wavetable".into();
    project.master = Project::new("").master;
    let device = project.tracks[0].devices[0].id;
    let model = project.device_mut(device).expect("the synth");
    let mut state = trill_dsp::wavetable::State::from_device_state(model.state.as_deref()).unwrap_or_default();
    let recipe = r#"{ "name": "Thin Reed", "size": 32,
        "frames": [ { "shape": "pulse", "width": 0.5 }, { "shape": "pulse", "width": 0.04, "tilt": -3 } ] }"#;
    state.osc2 = Some(trill_dsp::wavetable::TableSource::Recipe(serde_json::from_str(recipe).expect("a recipe")));
    model.state = Some(state.to_bytes());
    model.params.insert("osc2_table".into(), 0.0);
    model.params.insert("osc2_position".into(), 0.35);
    project
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let Some(folder) = arguments.next() else {
        anyhow::bail!("usage: wavetable_listen <DIR> [<PROJECT.trill> for the pictures]");
    };
    let folder = Path::new(&folder);
    std::fs::create_dir_all(folder)?;

    let descriptors = BuiltinFactory.descriptors();
    let synth = descriptors.iter().find(|descriptor| descriptor.uri == SYNTH).expect("the wavetable synth");
    let mut list = String::from(
        "# The wavetable synth, to listen to\n\n\
         Nobody has heard these files: the presets and the phrases were written by someone who cannot hear, from \
         how such sounds are made, and the presets were matched in level by measurement. Every file is the synth \
         alone, with no effect and no limiter after it, unless its name says otherwise.\n\n\
         Made by `cargo run -p trill-app --example wavetable_listen -- <this folder>`; the same on every run.\n\n\
         ## Each preset alone\n\n\
         | File | Preset | Kind | What it plays | Seconds | Peak |\n|---|---|---|---|---|---|\n",
    );
    let mut said = String::new();
    for (number, preset) in synth.presets.iter().filter(|preset| preset.name != "init").enumerate() {
        let phrase = phrase(&preset.name);
        let stem = format!("{:02} {}", number + 1, preset.name);
        let file = format!("{stem}.wav");
        let (seconds, peak) = write(&alone(&preset.name, false), &folder.join(&file))?;
        writeln!(
            list,
            "| `{file}` | {} | {} | {} at {} BPM | {seconds:.0} | {peak} |",
            preset.name, preset.category, phrase.plays, phrase.tempo
        )?;
        if preset.category != "Bass" {
            let file = format!("{stem} (with effects).wav");
            let (seconds, peak) = write(&alone(&preset.name, true), &folder.join(&file))?;
            let through = match preset.category.as_str() {
                "Lead" | "Pluck" => "a dotted-eighth delay and a reverb",
                _ => "a chorus and a reverb",
            };
            writeln!(
                list,
                "| `{file}` | {} | {} | the same, through {through} | {seconds:.0} | {peak} |",
                preset.name, preset.category
            )?;
        }
        writeln!(said, "- **{}** ({}): {}.", preset.name, preset.category, preset.description)?;
        println!("{stem}");
    }
    list.push_str(
        "\n## Several together\n\n\
         Mixed as a song is: track faders, a reverb every part is sent to, a delay on some, and the limiter a new \
         song has on its master.\n\n| File | What it is | Seconds | Peak |\n|---|---|---|---|\n",
    );
    let pieces: [(&str, Project, &str); 2] = [
        (
            "Piece 1 Night Drive.wav",
            night_drive(),
            "124 BPM, sixteen bars in E minor. Glass Pluck and Glass Pad begin; Solid Bass and the built-in drum \
             kit come in at bar 5; Anthem Saws play the chords from bar 9, their Bright macro opening; Sync Lead \
             has the tune from bar 13",
        ),
        (
            "Piece 2 Slow Glass.wav",
            slow_glass(),
            "84 BPM, twelve bars. Tine Piano plays the chords, PWM Strings hold them, Deep Sub has the roots, \
             String Pluck the tune; Choir comes in at bar 5 and changes its vowel to the end",
        ),
    ];
    for (file, project, what) in pieces {
        let (seconds, peak) = write(&project, &folder.join(file))?;
        writeln!(list, "| `{file}` | {what} | {seconds:.0} | {peak} |")?;
        println!("{file}");
    }
    write!(list, "\n## What each preset says of itself\n\n{said}")?;
    std::fs::write(folder.join("LIST.md"), list)?;

    if let Some(project) = arguments.next() {
        std::fs::write(&project, for_pictures().to_json())?;
        println!("{project}");
    }
    Ok(())
}
